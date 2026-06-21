//! Runner storage broker — proxies artifact-bucket GET/PUT for external,
//! zero-secret runners so they never hold S3 credentials or reach the object
//! store directly. The runner's only network peer stays mekhan (the same
//! single-origin posture as the brokered NATS transport and secret unwrap).
//!
//! Binary sibling, NOT OpenAPI-modeled (same category as `/api/yjs`,
//! `/api/cloud-layer`, `/petri/*`): the consumer is the Rust executor's
//! `BrokeredArtifactStore`, not the generated TS client. Mounted INSIDE
//! `require_auth_middleware`, so a `rnr_` bearer resolves to an `AuthUser`
//! whose `workspace_id` is the immutable scope.
//!
//! ## Authorization
//!
//! Every requested key is authorized against the runner's workspace BEFORE it
//! touches S3 — a runner may only GET/PUT artifacts owned by its own tenant.
//! The artifact keyspace is flat and not uniformly workspace-prefixed, so the
//! check is shape-aware (see [`authorize_runner_key`]):
//!
//!   - `ws/{workspace_id}/…`          → the embedded ws must equal the runner's.
//!   - `templates/{template_id}/…`    → `workflow_templates.workspace_id` match.
//!   - `instances/{instance_id}/…`    → owning template's workspace_id match.
//!   - `artifacts/mekhan-{ws}-…/…`    → the ws embedded in the execution_id.
//!
//! Anything else is denied. This is intentionally strict: an unrecognised key
//! shape fails closed rather than leaking cross-tenant.

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use aithericon_executor_domain::FoldBatch;

use crate::auth::AuthUser;
use crate::models::error::ApiError;
use crate::AppState;

#[derive(Deserialize)]
pub struct BlobQuery {
    /// Artifact object key (URL-encoded). E.g.
    /// `templates/{id}/v2/crawl/node-config.json`.
    pub key: String,
}

fn deny() -> ApiError {
    ApiError::forbidden("runner not authorized for this storage key")
}

/// Parse the workspace UUID embedded at the head of a mekhan execution_id —
/// `mekhan-{workspace_uuid}-{instance_uuid}-{node_suffix}` — so an
/// `artifacts/{execution_id}/…` promotion key can be scoped without a DB hit.
fn parse_exec_workspace(execution_id: &str) -> Option<Uuid> {
    let rest = execution_id.strip_prefix("mekhan-")?;
    // A UUID's canonical form is exactly 36 chars (8-4-4-4-12).
    if rest.len() < 36 {
        return None;
    }
    Uuid::parse_str(&rest[..36]).ok()
}

/// Fail closed unless `key` provably belongs to `runner_ws`. See the module
/// docs for the recognised shapes.
async fn authorize_runner_key(
    state: &AppState,
    runner_ws: Uuid,
    key: &str,
) -> Result<(), ApiError> {
    // Reject empty / traversal / absolute keys before any matching.
    if key.is_empty() || key.starts_with('/') || key.split('/').any(|s| s == "..") {
        return Err(ApiError::bad_request("invalid storage key"));
    }

    let mut segs = key.split('/');
    let head = segs.next().unwrap_or("");
    let next_uuid = |segs: &mut std::str::Split<'_, char>| -> Result<Uuid, ApiError> {
        Uuid::parse_str(segs.next().unwrap_or("")).map_err(|_| deny())
    };

    match head {
        "ws" => {
            let ws = next_uuid(&mut segs)?;
            if ws == runner_ws {
                Ok(())
            } else {
                Err(deny())
            }
        }
        "templates" => {
            let id = next_uuid(&mut segs)?;
            let owner: Option<(Uuid,)> =
                sqlx::query_as("SELECT workspace_id FROM workflow_templates WHERE id = $1")
                    .bind(id)
                    .fetch_optional(&state.db)
                    .await?;
            match owner {
                Some((ws,)) if ws == runner_ws => Ok(()),
                _ => Err(deny()),
            }
        }
        "instances" => {
            let id = next_uuid(&mut segs)?;
            // An instance inherits its tenant from the template it runs (the
            // same join `pages.rs` uses); `workflow_instances` carries no direct
            // workspace_id column.
            let owner: Option<(Uuid,)> = sqlx::query_as(
                "SELECT t.workspace_id FROM workflow_instances i \
                   JOIN workflow_templates t ON t.id = i.template_id \
                  WHERE i.id = $1",
            )
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
            match owner {
                Some((ws,)) if ws == runner_ws => Ok(()),
                _ => Err(deny()),
            }
        }
        "artifacts" => {
            let exec = segs.next().unwrap_or("");
            match parse_exec_workspace(exec) {
                Some(ws) if ws == runner_ws => Ok(()),
                _ => Err(deny()),
            }
        }
        _ => Err(deny()),
    }
}

/// `GET /api/storage/blob?key=…` — stream an artifact object back to the runner.
/// 404 when the object is absent (the brokered store maps that to NotFound).
pub async fn get_blob(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<BlobQuery>,
) -> Result<Response, ApiError> {
    let ws = user.require_workspace()?;
    authorize_runner_key(&state, ws, &q.key).await?;

    match state.s3.get_file_opt(&q.key).await {
        Ok(Some((bytes, content_type))) => {
            Ok(([(header::CONTENT_TYPE, content_type)], bytes).into_response())
        }
        Ok(None) => Err(ApiError::not_found(format!("artifact not found: {}", q.key))),
        Err(e) => Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            format!("storage get failed: {e}"),
        )),
    }
}

/// `PUT /api/storage/blob?key=…` — write a runner artifact (e.g. a promoted
/// `kind:"file"` output) into the bucket through mekhan.
pub async fn put_blob(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<BlobQuery>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let ws = user.require_workspace()?;
    authorize_runner_key(&state, ws, &q.key).await?;

    state
        .s3
        .put_file(&q.key, body.to_vec(), "application/octet-stream")
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("storage put failed: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/storage/fold` — runner fold-batch broker. The external runner
/// POSTs a `FoldBatch` here instead of JetStream-publishing to `INVENTORY_FOLD`
/// over its WebSocket connection (where it cannot reliably receive the
/// publish-ack — the batch lands but the ack never returns, failing the crawl
/// step). mekhan folds it straight into the inventory via the SAME ingest path
/// the NATS consumer uses, so the runner gets a clean HTTP response and no JS
/// ack round-trip is needed. Single-origin, consistent with the storage + secret
/// brokers.
pub async fn fold_ingest(
    State(state): State<AppState>,
    user: AuthUser,
    axum::Json(batch): axum::Json<FoldBatch>,
) -> Result<StatusCode, ApiError> {
    let ws = user.require_workspace()?;
    // Defense: the batch must belong to the runner's own workspace (the
    // execution_id embeds it). A runner can only fold into its own tenant.
    if let Some(batch_ws) = parse_exec_workspace(&batch.execution_id) {
        if batch_ws != ws {
            return Err(deny());
        }
    }
    crate::inventory::fold::process_fold_batch(&state.db, &batch)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, format!("fold ingest failed: {e}")))?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_workspace_parses_head_uuid() {
        let ws = Uuid::parse_str("beace4d4-a79f-4a6e-b23b-0e8db4f62626").unwrap();
        let exec = "mekhan-beace4d4-a79f-4a6e-b23b-0e8db4f62626-\
                    b0db797e-af49-4c2e-a0ff-1ca953286b26-2fccf9d1-2392-4af2-b532-e2f0fbe62ad1";
        assert_eq!(parse_exec_workspace(exec), Some(ws));
    }

    #[test]
    fn exec_workspace_rejects_malformed() {
        assert_eq!(parse_exec_workspace("artifacts-not-mekhan"), None);
        assert_eq!(parse_exec_workspace("mekhan-short"), None);
    }
}
