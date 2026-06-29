//! Service-account token verifier — the workspace-owned machine credential path.
//!
//! A service account authenticates with `Authorization: Bearer sat_{id}.{secret}`.
//! Unlike the `uat_` human PAT — which reconstructs the OWNING human and
//! re-validates their LIVE membership — a service account is OWNED BY THE
//! WORKSPACE: its workspace + role are FIXED by the `service_accounts` row, so it
//! survives the offboarding of any member. It is also distinct from the
//! `rnr_`/`wkr_` control-plane tokens in that it carries a workspace ROLE.
//!
//!   1. Parse the `sat_` prefix → `(token_id, secret)`.
//!   2. Fetch the token row JOINed to its service account (rejecting revoked
//!      tokens, expired tokens, and disabled service accounts).
//!   3. Constant-time compare `sha256(secret) == token_hash`.
//!   4. Best-effort `last_used_at` touch (fire-and-forget).
//!   5. Return an [`AuthUser`] representing the SA principal — subject
//!      `service-account:{sa_id}`, the SA's `workspace_id` + `role`, and a
//!      `"service-account"` marker role so the machine-principal gate can refuse
//!      it from SA-management endpoints.
//!
//! Resolves entirely against the local tables (offline in `dev_noop`). There is
//! NO `apply_override`: the workspace is deterministic (offboarding-proof), so an
//! attached active-workspace cookie can never re-scope a `sat_` principal.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use super::model::{AuthError, AuthUser};
use crate::models::runner::{parse_token, verify_secret, SERVICE_ACCOUNT_TOKEN_PREFIX};

/// Marker role stamped on a service-account principal — the machine marker the
/// SA-management gate (and any future roles-based check) reads, paralleling the
/// `"runner"` / `"worker"` markers.
pub const SERVICE_ACCOUNT_ROLE: &str = "service-account";

/// Build the `subject` string for a service-account principal:
/// `service-account:{id}`.
pub fn service_account_subject(id: uuid::Uuid) -> String {
    format!("service-account:{id}")
}

/// The token row JOINed to its owning service account, projected for
/// verification. `FromRow` maps by column NAME — the SELECT list below is the
/// contract, not the physical column order.
#[derive(Debug, Clone, sqlx::FromRow)]
struct SatAuthRow {
    token_hash: String,
    expires_at: Option<DateTime<Utc>>,
    sa_id: uuid::Uuid,
    workspace_id: uuid::Uuid,
    role: String,
    disabled_at: Option<DateTime<Utc>>,
}

/// Verify a `sat_{id}.{secret}` bearer credential against the
/// `service_account_tokens` table (JOINed to `service_accounts`).
///
/// On success returns an [`AuthUser`] representing the service account, scoped to
/// the SA's fixed workspace + role. Any structural failure, missing/revoked row,
/// expiry, hash mismatch, or disabled SA maps to a SINGLE opaque
/// [`AuthError::InvalidToken`] so the HTTP layer renders a uniform 401 whose body
/// never distinguishes the cause (the per-cause detail goes to a `debug!` log).
pub async fn verify_service_account_token(
    db: &PgPool,
    bearer: &str,
) -> Result<AuthUser, AuthError> {
    // Single client-facing reason for every user-caused failure; the real cause
    // is logged, never returned.
    fn reject(detail: &str) -> AuthError {
        tracing::debug!(reason = detail, "sat_ token rejected");
        AuthError::InvalidToken("invalid token".to_string())
    }

    let (token_id, secret) = parse_token(SERVICE_ACCOUNT_TOKEN_PREFIX, bearer)
        .ok_or_else(|| reject("malformed token"))?;

    let row = sqlx::query_as::<_, SatAuthRow>(
        "SELECT t.token_hash, t.expires_at, \
                sa.id AS sa_id, sa.workspace_id, sa.role, sa.disabled_at \
           FROM service_account_tokens t \
           JOIN service_accounts sa ON sa.id = t.service_account_id \
          WHERE t.id = $1 AND t.revoked_at IS NULL",
    )
    .bind(token_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AuthError::Internal(e.to_string()))?
    .ok_or_else(|| reject("unknown or revoked token id"))?;

    if let Some(exp) = row.expires_at {
        if exp <= Utc::now() {
            return Err(reject("expired token"));
        }
    }

    if !verify_secret(&secret, &row.token_hash) {
        return Err(reject("secret mismatch"));
    }

    if row.disabled_at.is_some() {
        return Err(reject("service account disabled"));
    }

    // Best-effort, fire-and-forget last-use touch. A failure here must never
    // block the request (mirrors the `uat_` path).
    let _ = sqlx::query("UPDATE service_account_tokens SET last_used_at = now() WHERE id = $1")
        .bind(token_id)
        .execute(db)
        .await;

    let subject = service_account_subject(row.sa_id);
    Ok(AuthUser {
        user_id: AuthUser::legacy_subject_uuid(&subject),
        subject,
        email: None,
        display_name: None,
        // Machine marker — paralleling runner/worker; the SA-management gate
        // refuses any principal carrying a machine subject prefix.
        roles: vec![SERVICE_ACCOUNT_ROLE.to_string()],
        is_platform_admin: false,
        // Workspace + role are FIXED by the SA row — no apply_override.
        workspace_id: Some(row.workspace_id),
        workspace_role: Some(row.role),
        avatar_url: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_is_service_account_prefixed() {
        let id = uuid::Uuid::new_v4();
        let s = service_account_subject(id);
        assert_eq!(s, format!("service-account:{id}"));
        assert!(s.starts_with("service-account:"));
    }
}
