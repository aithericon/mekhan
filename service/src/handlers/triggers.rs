//! HTTP handlers for the trigger API (Phase 5 of typed-ports).
//!
//! Endpoints:
//! - GET    `/api/v1/triggers`                       — list all registered triggers
//! - GET    `/api/v1/templates/{id}/triggers`        — list triggers per template
//! - POST   `/api/v1/triggers/{node_id}/fire`        — manual fire (Phase 5a)
//! - GET    `/api/v1/triggers/{node_id}/history`     — recent fire history
//!
//! Webhook receiver lives under `/api/triggers/webhook/{slug}` and lands in
//! Phase 5e.

use std::collections::HashMap;

use std::time::Duration;

use axum::{
    extract::{FromRequest, Multipart, Path, Query, Request, State},
    http::{header, HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::{
    HttpMethod, ReplyMode, TriggerSource, WorkflowGraph, WorkflowNodeData, WorkflowTemplate,
};
use crate::triggers::{FireOutcome, FireResult, TerminalOutcome, TriggerError, TriggerRecord};
use crate::AppState;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TriggerView {
    pub template_id: Uuid,
    pub template_version: i32,
    pub node_id: String,
    pub kind: String,
    pub target_node_id: String,
    pub target_handle: String,
    pub source_kind: String,
    pub enabled: bool,
    pub registered_at: DateTime<Utc>,
}

impl From<TriggerRecord> for TriggerView {
    fn from(rec: TriggerRecord) -> Self {
        let kind = match rec.kind {
            crate::triggers::TriggerKind::Spawn => "spawn".to_string(),
            crate::triggers::TriggerKind::Signal => "signal".to_string(),
        };
        let source_kind = rec.source.kind().to_string();
        Self {
            template_id: rec.template_id,
            template_version: rec.template_version,
            node_id: rec.node_id,
            kind,
            target_node_id: rec.target_node_id,
            target_handle: rec.target_handle,
            source_kind,
            enabled: rec.enabled,
            registered_at: rec.registered_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TriggerListResponse {
    pub triggers: Vec<TriggerView>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct FireTriggerRequest {
    /// JSON object whose top-level keys are bound as the trigger's scope
    /// identifiers for `payload_mapping`. For `Manual` triggers supply the
    /// form values keyed by field name (matching `source_scope`); for other
    /// sources the dispatcher synthesizes this scope from the event itself.
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Optional per-request reply mode (lowest of the explicit selectors —
    /// overridden by `?reply=` / `Prefer`, overrides the node default).
    #[serde(default)]
    pub reply_mode: Option<ReplyMode>,
}

/// Query selector for the reply mode: `?reply=wait|nowait|stream`.
#[derive(Debug, Default, Deserialize)]
pub struct FireQuery {
    #[serde(default)]
    pub reply: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FireTriggerResponse {
    pub result: FireResult,
    /// Present only on a successful `WaitForResult` fire — the instance's
    /// terminal disposition. Absent (not `null`) for FireAndForget, keeping
    /// the default response byte-identical to pre-feature behavior.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TerminalOutcome>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TriggerHistoryResponse {
    pub history: Vec<FireResult>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CronPreviewRequest {
    pub schedule: String,
    #[serde(default = "default_tz")]
    pub timezone: String,
    /// Number of upcoming fire times to compute (clamped to 1..=10).
    #[serde(default = "default_count")]
    pub count: u32,
}

fn default_tz() -> String {
    "UTC".to_string()
}
fn default_count() -> u32 {
    5
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CronPreviewResponse {
    /// Upcoming fire times in RFC 3339 (UTC).
    pub upcoming: Vec<String>,
    /// Error message if the schedule or timezone is invalid; `upcoming` is
    /// empty when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// GET /api/v1/triggers
#[utoipa::path(
    get,
    path = "/api/v1/triggers",
    responses(
        (status = 200, description = "All registered triggers", body = TriggerListResponse),
    ),
    tag = "triggers",
)]
pub async fn list_triggers(State(state): State<AppState>) -> Json<TriggerListResponse> {
    let triggers: Vec<TriggerView> = state
        .triggers
        .list_all()
        .into_iter()
        .map(TriggerView::from)
        .collect();
    Json(TriggerListResponse { triggers })
}

/// GET /api/v1/templates/{id}/triggers
#[utoipa::path(
    get,
    path = "/api/v1/templates/{id}/triggers",
    params(("id" = Uuid, Path, description = "Template id")),
    responses(
        (status = 200, description = "Triggers for this template", body = TriggerListResponse),
    ),
    tag = "triggers",
)]
pub async fn list_template_triggers(
    State(state): State<AppState>,
    Path(template_id): Path<Uuid>,
) -> Json<TriggerListResponse> {
    let triggers: Vec<TriggerView> = state
        .triggers
        .list_for_template(template_id)
        .into_iter()
        .map(TriggerView::from)
        .collect();
    Json(TriggerListResponse { triggers })
}

/// POST /api/v1/triggers/{node_id}/fire
///
/// Accepts either `application/json` (`{ "payload": { ... } }` — the scope
/// keys for `payload_mapping`) or `multipart/form-data` for file entrypoints:
/// an optional JSON `payload` part plus one binary part per file field. Each
/// file part is uploaded to blob storage (scoped to the trigger's template +
/// target node) and injected into the payload under the part name as a
/// `{ key, url, filename, content_type, size }` reference object — the same
/// shape the create-instance dialog produces, which `FieldKind::File` accepts.
/// Resolve the effective reply mode. Precedence (first match wins):
/// 1. `Accept: text/event-stream` ⇒ `Sse` (rejected with 406 — SSE is the
///    dedicated stream endpoint, not negotiated on fire)
/// 2. `?reply=wait|nowait|stream`
/// 3. `Prefer: respond-async` ⇒ FireAndForget
/// 4. JSON body `reply_mode`
/// 5. the Trigger node's `replyDefault`
/// 6. FireAndForget (back-compat default)
fn resolve_reply_mode(
    accept: &str,
    prefer: &str,
    query_reply: Option<&str>,
    body: Option<ReplyMode>,
    node_default: Option<ReplyMode>,
) -> ReplyMode {
    if accept.contains("text/event-stream") {
        return ReplyMode::Sse;
    }
    match query_reply {
        Some("wait") => return ReplyMode::WaitForResult,
        Some("nowait") => return ReplyMode::FireAndForget,
        Some("stream") => return ReplyMode::Sse,
        _ => {}
    }
    if prefer.contains("respond-async") {
        return ReplyMode::FireAndForget;
    }
    body.or(node_default).unwrap_or(ReplyMode::FireAndForget)
}

#[utoipa::path(
    post,
    path = "/api/v1/triggers/{node_id}/fire",
    params(("node_id" = String, Path, description = "Trigger node id")),
    request_body = FireTriggerRequest,
    responses(
        (status = 200, description = "Trigger fired (FireAndForget, or WaitForResult resolved)", body = FireTriggerResponse),
        (status = 202, description = "WaitForResult timed out — instance still running; poll/stream it"),
        (status = 404, description = "Trigger not found", body = ErrorResponse),
        (status = 406, description = "SSE requested on the fire endpoint — use GET /api/v1/instances/{id}/stream"),
        (status = 400, description = "Fire failed (e.g. mapping or instance error)", body = ErrorResponse),
    ),
    tag = "triggers",
)]
pub async fn fire_trigger(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(node_id): Path<String>,
    request: Request,
) -> Result<Response, ApiError> {
    // Read everything needed off the request before the body is consumed.
    let headers = request.headers();
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let accept = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let prefer = headers
        .get("prefer")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let query: FireQuery = Query::try_from_uri(request.uri())
        .map(|Query(q)| q)
        .unwrap_or_default();

    let (payload, body_reply_mode) = if content_type.starts_with("multipart/form-data") {
        (build_multipart_payload(&state, &node_id, request).await?, None)
    } else {
        let bytes = axum::body::to_bytes(request.into_body(), 4 * 1024 * 1024)
            .await
            .map_err(|e| ApiError::bad_request(format!("failed to read body: {e}")))?;
        if bytes.is_empty() {
            (Value::Null, None)
        } else {
            let req: FireTriggerRequest = serde_json::from_slice(&bytes)
                .map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))?;
            (req.payload, req.reply_mode)
        }
    };

    let node_default = state.triggers.get(&node_id).and_then(|r| r.reply_default);
    let mode = resolve_reply_mode(
        &accept,
        &prefer,
        query.reply.as_deref(),
        body_reply_mode,
        node_default,
    );

    match mode {
        ReplyMode::Sse => Err(ApiError::new(
            StatusCode::NOT_ACCEPTABLE,
            "SSE is served by GET /api/v1/instances/{id}/stream — fire FireAndForget, \
             then open the stream with the returned instance id",
        )),
        ReplyMode::FireAndForget => {
            let result =
                crate::triggers::sources::manual::fire(&state.triggers, &node_id, payload)
                    .await
                    .map_err(map_trigger_error)?;
            Ok(Json(FireTriggerResponse {
                result,
                outcome: None,
            })
            .into_response())
        }
        ReplyMode::WaitForResult => {
            let (result, rx) = crate::triggers::sources::manual::fire_waiting(
                &state.triggers,
                &node_id,
                payload,
                &state.result_waiters,
            )
            .await
            .map_err(map_trigger_error)?;

            match (&result.outcome, rx) {
                (FireOutcome::Spawned { instance_id }, Some(rx)) => {
                    let iid = *instance_id;
                    let dur = Duration::from_secs(state.config.wait_timeout_secs);
                    match tokio::time::timeout(dur, rx).await {
                        Ok(Ok(outcome)) => Ok(Json(FireTriggerResponse {
                            result,
                            outcome: Some(outcome),
                        })
                        .into_response()),
                        // Timeout, or sender dropped without sending: drop the
                        // waiter (no leak) and degrade to polling.
                        _ => {
                            state.result_waiters.deregister(&iid);
                            Ok((
                                StatusCode::ACCEPTED,
                                Json(serde_json::json!({ "instance_id": iid })),
                            )
                                .into_response())
                        }
                    }
                }
                // Signal-kind / dropped fire — no instance to wait on; return
                // the FireAndForget shape unchanged.
                _ => Ok(Json(FireTriggerResponse {
                    result,
                    outcome: None,
                })
                .into_response()),
            }
        }
    }
}

/// Build the fire payload from a `multipart/form-data` body: merge the JSON
/// `payload` part with one uploaded-file reference per remaining part. Files
/// land in blob storage scoped to the trigger's template + target node so
/// they're retrievable via `/api/v1/files/{key}` exactly like create-instance
/// uploads, and the injected reference object is accepted as-is by a `file`
/// port field (`FieldKind::File` accepts an object).
async fn build_multipart_payload(
    state: &AppState,
    node_id: &str,
    request: Request,
) -> Result<Value, ApiError> {
    let record = state.triggers.get(node_id).ok_or_else(|| {
        ApiError::not_found(format!(
            "trigger '{node_id}' not found in any published template"
        ))
    })?;

    let mut multipart = Multipart::from_request(request, state)
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart body: {e}")))?;

    let mut payload = serde_json::Map::new();
    // File parts are collected separately and applied LAST so an uploaded
    // binary always wins its field name. Previously each part was inserted
    // inline, making the outcome depend on multipart stream order: a JSON
    // `payload` part arriving after a file part `extend`-clobbered the file
    // ref with a colliding scalar (root cause of instance 6f347648 — curl
    // sent `-F invoice_file=@…` then `-F payload={"invoice_file":"example"}`,
    // and the scalar silently shadowed the upload).
    let mut file_parts: Vec<(String, Value)> = Vec::new();

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::bad_request(format!("invalid multipart field: {e}")))?
    {
        let Some(name) = field.name().map(str::to_string) else {
            continue;
        };
        if name.is_empty() {
            continue;
        }

        // The `payload` part carries the non-file scope keys as a JSON object.
        if name == "payload" {
            let text = field
                .text()
                .await
                .map_err(|e| ApiError::bad_request(format!("'payload' part: {e}")))?;
            if text.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(&text)
                .map_err(|e| ApiError::bad_request(format!("'payload' part is not JSON: {e}")))?
            {
                Value::Object(m) => payload.extend(m),
                _ => {
                    return Err(ApiError::bad_request(
                        "'payload' part must be a JSON object",
                    ))
                }
            }
            continue;
        }

        // Every other part is an uploaded file → store it and inject a
        // reference object under the part name.
        let content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();
        let filename = field.file_name().unwrap_or("upload.bin").to_string();
        let bytes = field
            .bytes()
            .await
            .map_err(|e| ApiError::bad_request(format!("file part '{name}': {e}")))?;
        let size = bytes.len();
        let key = state
            .s3
            .upload_blob(
                record.template_id,
                &record.target_node_id,
                &filename,
                &bytes,
                &content_type,
            )
            .await
            .map_err(|e| ApiError::internal(format!("upload failed: {e}")))?;

        file_parts.push((
            name,
            serde_json::json!({
                "key": key,
                "url": format!("/api/v1/files/{key}"),
                "filename": filename,
                "content_type": content_type,
                "size": size,
            }),
        ));
    }

    // Apply file refs after the JSON `payload` keys: the multipart file part
    // is authoritative for its field name, so a colliding scalar in `payload`
    // cannot shadow the upload regardless of the order parts arrived in. The
    // strict Start-contract gate at ingestion then rejects the inverse case
    // (no file part, only a scalar where a `file` field is declared).
    for (name, fref) in file_parts {
        payload.insert(name, fref);
    }

    Ok(Value::Object(payload))
}

/// GET /api/v1/triggers/{node_id}/history
#[utoipa::path(
    get,
    path = "/api/v1/triggers/{node_id}/history",
    params(("node_id" = String, Path, description = "Trigger node id")),
    responses(
        (status = 200, description = "Recent fire history", body = TriggerHistoryResponse),
    ),
    tag = "triggers",
)]
pub async fn trigger_history(
    State(state): State<AppState>,
    Path(node_id): Path<String>,
) -> Json<TriggerHistoryResponse> {
    let history = state.triggers.history_for(&node_id);
    Json(TriggerHistoryResponse { history })
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTriggerEnabledRequest {
    pub enabled: bool,
}

/// PATCH /api/v1/triggers/{node_id}/enabled
///
/// Arm or pause a single trigger on its **published** template. This is the
/// deliberate inverse of the rest of the template: a trigger's `source`,
/// `payload_mapping` and target are frozen at publish, but whether it is
/// currently armed is operational state of the live template — so it is
/// editable exactly when everything else is locked, and *only* then (drafts
/// are armed by editing the graph in the editor).
///
/// The published row's `graph` column is the runtime source of truth that
/// `hydrate()` and `fire()` read, so we flip `enabled` on the trigger node
/// there and re-register the template to make the change live immediately;
/// it then survives restarts via the normal `hydrate()` path.
#[utoipa::path(
    patch,
    path = "/api/v1/triggers/{node_id}/enabled",
    params(("node_id" = String, Path, description = "Trigger node id")),
    request_body = SetTriggerEnabledRequest,
    responses(
        (status = 200, description = "Trigger enabled state updated", body = TriggerView),
        (status = 404, description = "Trigger not found", body = ErrorResponse),
        (status = 409, description = "Template is not published", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "triggers",
)]
pub async fn set_trigger_enabled(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(node_id): Path<String>,
    Json(req): Json<SetTriggerEnabledRequest>,
) -> Result<Json<TriggerView>, ApiError> {
    let record = state.triggers.get(&node_id).ok_or_else(|| {
        ApiError::not_found(format!(
            "trigger '{node_id}' not found in any published template"
        ))
    })?;

    let template = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1 AND version = $2",
    )
    .bind(record.template_id)
    .bind(record.template_version)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .ok_or_else(|| ApiError::not_found("template not found"))?;

    // Inverse of the publish freeze: enable/disable is *only* a published-
    // template control. A draft trigger's armed state is set by editing the
    // graph in the editor, so reject this here to keep one writer per state.
    if !template.published {
        return Err(ApiError::conflict(
            "trigger enable/disable is only available on a published template; edit drafts in the editor",
        ));
    }

    let mut graph: WorkflowGraph = serde_json::from_value(template.graph.clone())
        .map_err(|e| ApiError::internal(format!("invalid graph: {e}")))?;

    let mut found = false;
    for node in &mut graph.nodes {
        if node.id != node_id {
            continue;
        }
        if let WorkflowNodeData::Trigger { enabled, .. } = &mut node.data {
            *enabled = req.enabled;
            found = true;
        }
    }
    if !found {
        return Err(ApiError::not_found(format!(
            "trigger node '{node_id}' not present in template graph"
        )));
    }

    let graph_json =
        serde_json::to_value(&graph).map_err(|e| ApiError::internal(e.to_string()))?;

    let updated = sqlx::query_as::<_, WorkflowTemplate>(
        r#"
        UPDATE workflow_templates
        SET graph = $2, updated_at = NOW()
        WHERE id = $1
        RETURNING *
        "#,
    )
    .bind(template.id)
    .bind(&graph_json)
    .fetch_one(&state.db)
    .await
    .map_err(|e| {
        tracing::error!("failed to persist trigger enabled state: {e}");
        ApiError::internal(e.to_string())
    })?;

    // Refresh the in-memory registry so the change is live without a restart.
    // `register_template` clears this template+version's prior records first,
    // so this is idempotent. We pass `do_backfill = true` because this path
    // also handles toggling a Catalog trigger from disabled→enabled, where
    // backfill might legitimately be wanted — the dispatcher's prior-id
    // snapshot still suppresses re-fires for triggers that didn't change.
    state.triggers.register_template(&updated, true).await;

    let view = state
        .triggers
        .get(&node_id)
        .map(TriggerView::from)
        .ok_or_else(|| ApiError::internal("trigger missing after re-register"))?;
    Ok(Json(view))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TriggerMetricsResponse {
    pub by_source_kind: HashMap<String, crate::triggers::dispatcher::FireMetrics>,
    pub total_registered: usize,
}

/// GET /api/v1/triggers/metrics
///
/// Returns aggregate counters per source kind plus the registry size. Useful
/// for /admin dashboards and the editor's trigger landing page.
#[utoipa::path(
    get,
    path = "/api/v1/triggers/metrics",
    responses(
        (status = 200, description = "Per-source-kind fire counters", body = TriggerMetricsResponse),
    ),
    tag = "triggers",
)]
pub async fn trigger_metrics(State(state): State<AppState>) -> Json<TriggerMetricsResponse> {
    Json(TriggerMetricsResponse {
        by_source_kind: state.triggers.metrics_snapshot(),
        total_registered: state.triggers.list_all().len(),
    })
}

/// POST /api/v1/triggers/preview/cron
///
/// Returns the next N fire times for a cron schedule. Used by the editor's
/// trigger inspector to show users when their cron will fire next without
/// having to ship the workflow first.
#[utoipa::path(
    post,
    path = "/api/v1/triggers/preview/cron",
    request_body = CronPreviewRequest,
    responses(
        (status = 200, description = "Upcoming fire times (or error)", body = CronPreviewResponse),
    ),
    tag = "triggers",
)]
pub async fn preview_cron(Json(req): Json<CronPreviewRequest>) -> Json<CronPreviewResponse> {
    use crate::models::template::{CronCatchup, CronTrigger};
    let count = req.count.clamp(1, 10) as usize;
    let trigger = CronTrigger {
        schedule: req.schedule,
        timezone: req.timezone,
        jitter_secs: 0,
        catchup: CronCatchup::SkipMissed,
    };
    match crate::triggers::sources::cron::parse_cron(&trigger) {
        Ok((schedule, tz)) => {
            let now = chrono::Utc::now().with_timezone(&tz);
            let upcoming: Vec<String> = schedule
                .after(&now)
                .take(count)
                .map(|t| t.with_timezone(&chrono::Utc).to_rfc3339())
                .collect();
            Json(CronPreviewResponse {
                upcoming,
                error: None,
            })
        }
        Err(msg) => Json(CronPreviewResponse {
            upcoming: vec![],
            error: Some(msg),
        }),
    }
}

#[derive(Debug, Deserialize)]
pub struct SourceScopeQuery {
    /// Source kind: `cron` | `catalog` | `net_completion` | `webhook` | `manual`.
    pub kind: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SourceScopeResponse {
    /// Identifiers a `payload_mapping` expression may reference for this source
    /// kind, with their declared kinds. `manual` returns empty — the editor
    /// derives that scope from the (client-side) form schema.
    pub scope: Vec<crate::triggers::ScopeVar>,
}

/// GET /api/v1/triggers/source-scope?kind=cron
///
/// The per-source scope contract, surfaced so the editor can show authors
/// exactly which identifiers are in scope under each mapping expression
/// instead of leaving them to guess.
#[utoipa::path(
    get,
    path = "/api/v1/triggers/source-scope",
    params(("kind" = String, Query, description = "Source kind: cron|catalog|net_completion|webhook|manual")),
    responses(
        (status = 200, description = "Available scope identifiers for the source kind", body = SourceScopeResponse),
    ),
    tag = "triggers",
)]
pub async fn trigger_source_scope(Query(q): Query<SourceScopeQuery>) -> Json<SourceScopeResponse> {
    Json(SourceScopeResponse {
        scope: crate::triggers::scope_for_kind(&q.kind),
    })
}

fn map_trigger_error(e: TriggerError) -> ApiError {
    match e {
        TriggerError::NotFound(_) => ApiError::not_found(e.to_string()),
        TriggerError::Disabled(_) => ApiError::new(StatusCode::CONFLICT, e.to_string()),
        TriggerError::Database(_) => ApiError::internal(e.to_string()),
        TriggerError::TargetMissing { .. }
        | TriggerError::PayloadMappingFailed { .. }
        | TriggerError::StartContractViolation { .. }
        | TriggerError::InstanceFailed(_)
        | TriggerError::SignalFailed(_) => ApiError::bad_request(e.to_string()),
    }
}

/// Convenience: utoipa schema registration helper. Exposes the FireResult /
/// FireOutcome / TriggerLocator wire shapes to the generated TS client.
#[allow(dead_code)]
pub fn schema_exports() {
    let _ = std::any::type_name::<crate::triggers::FireResult>();
    let _ = std::any::type_name::<crate::triggers::FireOutcome>();
    let _ = std::any::type_name::<crate::triggers::TriggerLocator>();
}

/// POST /api/triggers/webhook/{slug}
///
/// Public webhook receiver. The handler resolves the slug to a registered
/// webhook trigger, validates auth per the trigger's `WebhookAuth` policy,
/// then fires with a payload of `{ payload (body), headers, query, fire_time }`.
///
/// Mounted outside the auth middleware — external systems POST to this URL
/// without a Bearer token.
pub async fn webhook_receiver(
    State(state): State<AppState>,
    method: Method,
    Path(slug): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<FireTriggerResponse>, ApiError> {
    let trigger = crate::triggers::sources::webhook::find_by_slug(&state.triggers, &slug)
        .ok_or_else(|| ApiError::not_found(format!("webhook '{slug}' not found")))?;

    let TriggerSource::Webhook(ref webhook) = trigger.source else {
        return Err(ApiError::internal("trigger source is not a webhook"));
    };

    // Method restriction (optional). The default Webhook.require_method is
    // None which accepts every method.
    if let Some(req_method) = webhook.require_method {
        if !methods_match(req_method, &method) {
            return Err(ApiError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                format!("webhook requires {req_method:?}, got {method}"),
            ));
        }
    }

    let header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.as_str().to_string(), s.to_string()))
        })
        .collect();

    // Secret resolver: looks up secrets from app config. Phase 5e uses env
    // vars prefixed with `WEBHOOK_SECRET_` for simplicity — a real deployment
    // would point at a secret store.
    let resolver = |secret_ref: &str| -> Option<String> {
        std::env::var(format!("WEBHOOK_SECRET_{secret_ref}")).ok()
    };

    if let Err(msg) =
        crate::triggers::sources::webhook::check_auth(webhook, &header_map, body.as_ref(), resolver)
    {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, msg));
    }

    // Parse the body as JSON if Content-Type is application/json; otherwise
    // pass it as a base64-encoded blob under `body_bytes`.
    let body_payload: Value = if let Ok(v) = serde_json::from_slice::<Value>(body.as_ref()) {
        v
    } else {
        serde_json::json!({
            "body_bytes": format!("{}", String::from_utf8_lossy(body.as_ref())),
        })
    };

    let payload = serde_json::json!({
        "payload": body_payload,
        "headers": header_map,
        "query": query,
        "fire_time": Utc::now().to_rfc3339(),
    });

    let result = state
        .triggers
        .fire(&trigger.node_id, payload)
        .await
        .map_err(map_trigger_error)?;
    Ok(Json(FireTriggerResponse {
        result,
        outcome: None,
    }))
}

fn methods_match(declared: HttpMethod, actual: &Method) -> bool {
    match declared {
        HttpMethod::Get => actual == Method::GET,
        HttpMethod::Post => actual == Method::POST,
        HttpMethod::Put => actual == Method::PUT,
        HttpMethod::Patch => actual == Method::PATCH,
        HttpMethod::Delete => actual == Method::DELETE,
    }
}
