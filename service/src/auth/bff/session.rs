//! Server-side session custody. The opaque cookie value is a row id here; the
//! token set never leaves Postgres.
//!
//! `SessionStore` is a port (trait) so tests can substitute an in-memory
//! double; `PgSessionStore` is the production adapter. Login-flow rows
//! (in-flight PKCE state) live in the same store with their own short TTL.

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use rand::RngCore;
use sqlx::PgPool;

use crate::auth::model::{AuthError, AuthUser};

/// An established session row. `user` is the cached resolved domain principal
/// so the hot path never re-verifies a JWT.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub subject: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub access_expires_at: DateTime<Utc>,
    pub user: AuthUser,
}

/// An in-flight Authorization-Code + PKCE login, created at `/api/auth/login`
/// and consumed at `/api/auth/callback`.
#[derive(Debug, Clone)]
pub struct LoginFlow {
    pub state: String,
    pub pkce_verifier: String,
    pub nonce: String,
    pub return_to: String,
}

/// Fields updated in place when an access token is transparently refreshed.
#[derive(Debug, Clone)]
pub struct RefreshedTokens {
    pub access_token: String,
    /// Zitadel rotates refresh tokens; carry the new one when present, else
    /// keep the existing one.
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub access_expires_at: DateTime<Utc>,
}

#[async_trait]
pub trait SessionStore: Send + Sync {
    // ── established sessions ──────────────────────────────────────────────
    async fn create_session(
        &self,
        subject: &str,
        access_token: &str,
        refresh_token: Option<&str>,
        id_token: Option<&str>,
        access_expires_at: DateTime<Utc>,
        user: &AuthUser,
    ) -> Result<String, AuthError>;

    async fn get_session(&self, id: &str) -> Result<Option<Session>, AuthError>;

    async fn update_tokens(
        &self,
        id: &str,
        tokens: &RefreshedTokens,
    ) -> Result<(), AuthError>;

    async fn delete_session(&self, id: &str) -> Result<(), AuthError>;

    // ── in-flight login flows ─────────────────────────────────────────────
    async fn create_login_flow(&self, flow: &LoginFlow) -> Result<(), AuthError>;

    /// Atomically fetch-and-delete the login flow for `state` (single-use).
    async fn take_login_flow(&self, state: &str) -> Result<Option<LoginFlow>, AuthError>;

    /// Delete expired sessions and stale login flows. Returns rows removed.
    async fn sweep_expired(&self, session_ttl_secs: i64) -> Result<u64, AuthError>;
}

/// Postgres-backed session store.
#[derive(Clone)]
pub struct PgSessionStore {
    db: PgPool,
}

impl PgSessionStore {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

/// Opaque 256-bit session id, base64url (no padding). Unguessable and not
/// derived from any token, so a leaked cookie reveals nothing.
fn new_session_id() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn db_err(e: sqlx::Error) -> AuthError {
    AuthError::Internal(format!("session store: {e}"))
}

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn create_session(
        &self,
        subject: &str,
        access_token: &str,
        refresh_token: Option<&str>,
        id_token: Option<&str>,
        access_expires_at: DateTime<Utc>,
        user: &AuthUser,
    ) -> Result<String, AuthError> {
        let id = new_session_id();
        let user_json = serde_json::to_value(user)
            .map_err(|e| AuthError::Internal(format!("user serialize: {e}")))?;
        sqlx::query(
            r#"
            INSERT INTO auth_sessions
                (id, subject, access_token, refresh_token, id_token,
                 access_expires_at, user_json)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind(&id)
        .bind(subject)
        .bind(access_token)
        .bind(refresh_token)
        .bind(id_token)
        .bind(access_expires_at)
        .bind(&user_json)
        .execute(&self.db)
        .await
        .map_err(db_err)?;
        Ok(id)
    }

    async fn get_session(&self, id: &str) -> Result<Option<Session>, AuthError> {
        let row = sqlx::query_as::<_, SessionRow>(
            r#"
            SELECT id, subject, access_token, refresh_token, id_token,
                   access_expires_at, user_json
            FROM auth_sessions
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(db_err)?;

        let Some(row) = row else { return Ok(None) };

        // Touch last_seen_at out of band; failure here is non-fatal for the
        // request (it only affects idle-session sweeping precision).
        let _ = sqlx::query("UPDATE auth_sessions SET last_seen_at = NOW() WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await;

        let user: AuthUser = serde_json::from_value(row.user_json)
            .map_err(|e| AuthError::Internal(format!("user deserialize: {e}")))?;
        Ok(Some(Session {
            id: row.id,
            subject: row.subject,
            access_token: row.access_token,
            refresh_token: row.refresh_token,
            id_token: row.id_token,
            access_expires_at: row.access_expires_at,
            user,
        }))
    }

    async fn update_tokens(
        &self,
        id: &str,
        tokens: &RefreshedTokens,
    ) -> Result<(), AuthError> {
        // COALESCE keeps the existing refresh/id token when the IdP didn't
        // return a fresh one on this refresh.
        sqlx::query(
            r#"
            UPDATE auth_sessions
            SET access_token = $2,
                refresh_token = COALESCE($3, refresh_token),
                id_token = COALESCE($4, id_token),
                access_expires_at = $5,
                last_seen_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(id)
        .bind(&tokens.access_token)
        .bind(tokens.refresh_token.as_deref())
        .bind(tokens.id_token.as_deref())
        .bind(tokens.access_expires_at)
        .execute(&self.db)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn delete_session(&self, id: &str) -> Result<(), AuthError> {
        sqlx::query("DELETE FROM auth_sessions WHERE id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    async fn create_login_flow(&self, flow: &LoginFlow) -> Result<(), AuthError> {
        sqlx::query(
            r#"
            INSERT INTO auth_login_flows (state, pkce_verifier, nonce, return_to)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(&flow.state)
        .bind(&flow.pkce_verifier)
        .bind(&flow.nonce)
        .bind(&flow.return_to)
        .execute(&self.db)
        .await
        .map_err(db_err)?;
        Ok(())
    }

    async fn take_login_flow(&self, state: &str) -> Result<Option<LoginFlow>, AuthError> {
        // DELETE ... RETURNING is atomic single-use: a replayed callback finds
        // nothing the second time.
        let row = sqlx::query_as::<_, LoginFlowRow>(
            r#"
            DELETE FROM auth_login_flows
            WHERE state = $1
            RETURNING state, pkce_verifier, nonce, return_to
            "#,
        )
        .bind(state)
        .fetch_optional(&self.db)
        .await
        .map_err(db_err)?;
        Ok(row.map(|r| LoginFlow {
            state: r.state,
            pkce_verifier: r.pkce_verifier,
            nonce: r.nonce,
            return_to: r.return_to,
        }))
    }

    async fn sweep_expired(&self, session_ttl_secs: i64) -> Result<u64, AuthError> {
        let sessions = sqlx::query(
            r#"
            DELETE FROM auth_sessions
            WHERE created_at < NOW() - ($1 * INTERVAL '1 second')
            "#,
        )
        .bind(session_ttl_secs as f64)
        .execute(&self.db)
        .await
        .map_err(db_err)?
        .rows_affected();

        // Login flows are short-lived; 10 minutes is generous for a human
        // completing an IdP login.
        let flows = sqlx::query(
            "DELETE FROM auth_login_flows WHERE created_at < NOW() - INTERVAL '10 minutes'",
        )
        .execute(&self.db)
        .await
        .map_err(db_err)?
        .rows_affected();

        Ok(sessions + flows)
    }
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    id: String,
    subject: String,
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    access_expires_at: DateTime<Utc>,
    user_json: serde_json::Value,
}

#[derive(sqlx::FromRow)]
struct LoginFlowRow {
    state: String,
    pkce_verifier: String,
    nonce: String,
    return_to: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_unpadded_base64url_256bit() {
        let id = new_session_id();
        assert_eq!(id.len(), 43); // 32 bytes → 43 base64url chars
        assert!(!id.contains('='));
        assert!(id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn session_ids_are_unique() {
        let a = new_session_id();
        let b = new_session_id();
        assert_ne!(a, b);
    }
}
