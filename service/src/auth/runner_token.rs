//! Runner-token verifier — the mekhan-native control-plane credential path
//! (Phase 1, Lab Runner Fleet).
//!
//! A runner authenticates with `Authorization: Bearer rnr_{id}.{secret}`. This
//! is a non-human machine credential (distinct from the `uat_` user PAT) — it
//! resolves entirely against the local `runners` table, so it works offline in
//! `dev_noop`:
//!
//!   1. Parse the `rnr_` prefix → `(runner_id, secret)`.
//!   2. Fetch the `runners` row by id (rejecting revoked / soft-deleted rows).
//!   3. Constant-time compare `sha256(secret) == token_hash`.
//!   4. Return an [`AuthUser`] representing the runner principal.
//!
//! The runner principal's `subject` is `runner:{id}`, its `workspace_id` is the
//! runner's workspace, and its `roles` carry the `"runner"` marker so handlers
//! can authorize on a runner identity distinct from a human session.

use sqlx::PgPool;

use super::model::{AuthError, AuthUser};
use crate::models::runner::{parse_token, verify_secret, RunnerRow, RUNNER_TOKEN_PREFIX};

/// Marker role stamped on a runner principal.
pub const RUNNER_ROLE: &str = "runner";

/// Build the `subject` string for a runner principal: `runner:{id}`.
pub fn runner_subject(id: uuid::Uuid) -> String {
    format!("runner:{id}")
}

/// Verify a `rnr_{id}.{secret}` bearer credential against the `runners` table.
///
/// On success returns an [`AuthUser`] representing the runner. Any structural
/// failure, missing/revoked row, or hash mismatch maps to
/// [`AuthError::InvalidToken`] so the HTTP layer renders a uniform 401.
pub async fn verify_runner_token(db: &PgPool, bearer: &str) -> Result<AuthUser, AuthError> {
    let (runner_id, secret) = parse_token(RUNNER_TOKEN_PREFIX, bearer)
        .ok_or_else(|| AuthError::InvalidToken("malformed runner token".to_string()))?;

    let row = sqlx::query_as::<_, RunnerRow>(
        "SELECT * FROM runners WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(runner_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AuthError::Internal(e.to_string()))?
    .ok_or_else(|| AuthError::InvalidToken("unknown or revoked runner".to_string()))?;

    if !verify_secret(&secret, &row.token_hash) {
        return Err(AuthError::InvalidToken("runner token mismatch".to_string()));
    }

    let subject = runner_subject(row.id);
    Ok(AuthUser {
        user_id: AuthUser::legacy_subject_uuid(&subject),
        subject,
        email: None,
        display_name: Some(row.name),
        roles: vec![RUNNER_ROLE.to_string()],
        org_id: None,
        is_platform_admin: false,
        workspace_id: Some(row.workspace_id),
        workspace_role: None,
        avatar_url: None,
    })
}
