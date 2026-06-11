//! Worker-token verifier — the mekhan-native control-plane credential path for
//! enrolled workers (Phase A, Grouped + Enrolled Workers).
//!
//! The exact parallel of [`super::runner_token`] for the *worker* pool. A worker
//! authenticates with `Authorization: Bearer wkr_{id}.{secret}`. This is NOT a
//! Zitadel PAT and never touches introspection — it resolves entirely against the
//! local `workers` table, so it works offline in `dev_noop`:
//!
//!   1. Parse the `wkr_` prefix → `(worker_id, secret)`.
//!   2. Fetch the `workers` row by id (rejecting revoked / soft-deleted rows).
//!   3. Constant-time compare `sha256(secret) == token_hash`.
//!   4. Return an [`AuthUser`] representing the worker principal.
//!
//! The worker principal's `subject` is `worker:{id}`, its `workspace_id` is the
//! worker's workspace, and its `roles` carry the `"worker"` marker so handlers
//! can authorize on a worker identity distinct from a human session (or a runner).

use sqlx::PgPool;

use super::model::{AuthError, AuthUser};
use crate::models::runner::{parse_token, verify_secret};
use crate::models::worker::{WorkerRow, WORKER_TOKEN_PREFIX};

/// Marker role stamped on a worker principal.
pub const WORKER_ROLE: &str = "worker";

/// Build the `subject` string for a worker principal: `worker:{id}`.
pub fn worker_subject(id: uuid::Uuid) -> String {
    format!("worker:{id}")
}

/// Verify a `wkr_{id}.{secret}` bearer credential against the `workers` table.
///
/// On success returns an [`AuthUser`] representing the worker. Any structural
/// failure, missing/revoked row, or hash mismatch maps to
/// [`AuthError::InvalidToken`] so the HTTP layer renders a uniform 401.
pub async fn verify_worker_token(db: &PgPool, bearer: &str) -> Result<AuthUser, AuthError> {
    let (worker_id, secret) = parse_token(WORKER_TOKEN_PREFIX, bearer)
        .ok_or_else(|| AuthError::InvalidToken("malformed worker token".to_string()))?;

    let row = sqlx::query_as::<_, WorkerRow>(
        "SELECT * FROM workers WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(worker_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AuthError::Internal(e.to_string()))?
    .ok_or_else(|| AuthError::InvalidToken("unknown or revoked worker".to_string()))?;

    if !verify_secret(&secret, &row.token_hash) {
        return Err(AuthError::InvalidToken("worker token mismatch".to_string()));
    }

    Ok(AuthUser {
        subject: worker_subject(row.id),
        email: None,
        display_name: Some(row.name),
        roles: vec![WORKER_ROLE.to_string()],
        org_id: None,
        workspace_id: Some(row.workspace_id),
        workspace_role: None,
        avatar_url: None,
    })
}
