//! User Personal Access Token (PAT) verifier — the mekhan-native, offline
//! replacement for the retired Zitadel introspection Bearer path.
//!
//! A human mints a `uat_{id}.{secret}` credential via `/api/v1/auth/tokens`
//! (see [`crate::handlers::auth_tokens`]) and presents it as
//! `Authorization: Bearer uat_...` on non-interactive API calls (CI
//! `mekhan apply`). Unlike the runner/worker tokens — which resolve to a
//! *synthetic* non-human principal — a user PAT must reconstruct the OWNING
//! human exactly as a cookie session would, so the two are indistinguishable to
//! every downstream gate:
//!
//!   1. Parse the `uat_` prefix → `(pat_id, secret)`.
//!   2. Fetch the `user_pats` row by id (rejecting revoked rows + expired tokens).
//!   3. Constant-time compare `sha256(secret) == token_hash`.
//!   4. Load the owning `users` row (+ a representative identity subject), then
//!      replicate the resolver's platform-admin / membership / role stamping so
//!      the resulting [`AuthUser`] == the human's cookie principal.
//!   5. Best-effort `last_used_at` touch (fire-and-forget).
//!
//! This resolves entirely against the local `users` / `user_identities` /
//! `user_pats` tables, so it works offline in `dev_noop` and never touches an
//! IdP. It deliberately does NOT route through `PrincipalResolver` (which would
//! re-run per-request personal-workspace provisioning + identity reconcile on a
//! hot machine path) — it reads the already-resolved spine directly.

use chrono::{DateTime, Utc};

use super::model::{AuthError, AuthUser};
use crate::models::runner::{parse_token, verify_secret, USER_PAT_TOKEN_PREFIX};

/// One `user_pats` row, in the column order the migration declares.
#[derive(Debug, Clone, sqlx::FromRow)]
struct PatAuthRow {
    user_id: uuid::Uuid,
    token_hash: String,
    expires_at: Option<DateTime<Utc>>,
}

/// The owning human, joined from `users` (+ a representative identity subject).
#[derive(Debug, Clone, sqlx::FromRow)]
struct PatOwnerRow {
    email: Option<String>,
    display_name: Option<String>,
    avatar_url: Option<String>,
    subject: Option<String>,
}

/// Whether a principal is a platform admin per the config allow-list — matches
/// the subject OR the email against any entry. Mirrors `resolver.rs` so a PAT
/// principal gets the identical `is_platform_admin` flag as the cookie path.
pub(crate) fn is_platform_admin(
    platform_admins: &[String],
    subject: &str,
    email: Option<&str>,
) -> bool {
    platform_admins
        .iter()
        .any(|entry| entry == subject || email == Some(entry.as_str()))
}

/// Verify a `uat_{id}.{secret}` bearer credential against the `user_pats` table
/// and reconstruct the OWNING human principal.
///
/// On success returns an [`AuthUser`] indistinguishable from the owner's cookie
/// session (same `user_id`, workspace, role, platform-admin flag). Any
/// structural failure, missing/revoked row, expiry, or hash mismatch maps to
/// [`AuthError::InvalidToken`] so the HTTP layer renders a uniform 401.
pub async fn verify_user_pat(state: &crate::AppState, bearer: &str) -> Result<AuthUser, AuthError> {
    let (pat_id, secret) = parse_token(USER_PAT_TOKEN_PREFIX, bearer)
        .ok_or_else(|| AuthError::InvalidToken("malformed PAT".to_string()))?;

    let row = sqlx::query_as::<_, PatAuthRow>(
        "SELECT user_id, token_hash, expires_at \
           FROM user_pats WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(pat_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AuthError::Internal(e.to_string()))?
    .ok_or_else(|| AuthError::InvalidToken("unknown or revoked token".to_string()))?;

    if let Some(exp) = row.expires_at {
        if exp <= Utc::now() {
            return Err(AuthError::InvalidToken("expired token".to_string()));
        }
    }

    if !verify_secret(&secret, &row.token_hash) {
        return Err(AuthError::InvalidToken("token mismatch".to_string()));
    }

    let user_id = row.user_id;

    // Load the owning human + a representative identity subject (oldest link).
    let owner = sqlx::query_as::<_, PatOwnerRow>(
        "SELECT u.email, u.display_name, u.avatar_url, ui.subject \
           FROM users u \
           LEFT JOIN LATERAL ( \
               SELECT subject FROM user_identities ui \
                WHERE ui.user_id = u.id \
                ORDER BY ui.linked_at LIMIT 1 \
           ) ui ON TRUE \
          WHERE u.id = $1",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AuthError::Internal(e.to_string()))?
    .ok_or_else(|| AuthError::InvalidToken("token owner no longer exists".to_string()))?;

    let subject = owner.subject.unwrap_or_else(|| format!("user:{user_id}"));
    let is_platform_admin = is_platform_admin(
        &state.config.auth.platform_admins,
        &subject,
        owner.email.as_deref(),
    );

    let workspace_id = crate::auth::resolver::membership_workspace(&state.db, user_id).await?;
    let workspace_role = match workspace_id {
        Some(ws) => crate::auth::resolver::lookup_role(&state.db, ws, user_id).await?,
        None => None,
    };

    // Best-effort, fire-and-forget last-use touch. A failure here must never
    // block the request (mirrors the profile upsert on the cookie path).
    let _ = sqlx::query("UPDATE user_pats SET last_used_at = now() WHERE id = $1")
        .bind(pat_id)
        .execute(&state.db)
        .await;

    Ok(AuthUser {
        user_id,
        subject,
        email: owner.email,
        display_name: owner.display_name,
        // No runner/worker marker — this principal IS the human.
        roles: vec![],
        org_id: None,
        is_platform_admin,
        workspace_id,
        workspace_role,
        avatar_url: owner.avatar_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_admin_matches_subject_or_email() {
        let admins = vec!["alice@corp".to_string(), "user:sub-9".to_string()];
        assert!(is_platform_admin(&admins, "user:sub-9", None));
        assert!(is_platform_admin(&admins, "other", Some("alice@corp")));
        assert!(!is_platform_admin(&admins, "other", Some("bob@corp")));
        assert!(!is_platform_admin(&[], "user:sub-9", Some("alice@corp")));
    }
}
