//! Workspace service accounts — DB row structs + wire DTOs.
//!
//! A service account is a NON-human API principal OWNED BY A WORKSPACE. Unlike a
//! workspace-scoped human PAT (`uat_`), it is not tied to a member's identity, so
//! it survives member offboarding — it dies only when disabled or its token is
//! revoked. Its tokens are `sat_{id}.{secret}` credentials sharing the
//! mint/parse/verify helpers in [`crate::models::runner`]; only the SHA-256 of
//! the secret half is ever stored.
//!
//! DTOs deliberately OMIT `token_hash` and NEVER carry a token secret except in
//! [`CreatedServiceAccountToken`], which surfaces the full `sat_` token exactly
//! ONCE at mint.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// ── DB rows ────────────────────────────────────────────────────────────────

/// One row from the `service_accounts` table. `FromRow` maps by column NAME, so
/// the field order need not match the table — only the SELECT list.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ServiceAccountRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub role: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub disabled_at: Option<DateTime<Utc>>,
}

/// One row from the `service_account_tokens` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ServiceAccountTokenRow {
    pub id: Uuid,
    pub service_account_id: Uuid,
    pub name: String,
    /// SHA-256 (hex) of the secret half. NEVER leaves the server — DTOs omit it.
    pub token_hash: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

// ── Wire DTOs ──────────────────────────────────────────────────────────────

/// Service-account list/summary row. MUST NOT carry any secret material.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ServiceAccountSummary {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Fixed workspace role: `viewer` | `editor` | `admin` (never `owner`).
    pub role: String,
    pub created_at: DateTime<Utc>,
    /// Non-NULL ⇒ disabled; every token of a disabled SA is rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_at: Option<DateTime<Utc>>,
    /// The human admin who created the SA, or `None` if that account was deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
}

impl From<ServiceAccountRow> for ServiceAccountSummary {
    fn from(r: ServiceAccountRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            description: r.description,
            role: r.role,
            created_at: r.created_at,
            disabled_at: r.disabled_at,
            created_by: r.created_by,
        }
    }
}

/// Token metadata row. NEVER carries `token_hash` or the secret.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ServiceAccountTokenSummary {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
}

impl From<ServiceAccountTokenRow> for ServiceAccountTokenSummary {
    fn from(r: ServiceAccountTokenRow) -> Self {
        Self {
            id: r.id,
            name: r.name,
            created_at: r.created_at,
            expires_at: r.expires_at,
            last_used_at: r.last_used_at,
        }
    }
}

/// Request body for `POST /api/v1/workspaces/{workspace_id}/service-accounts`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateServiceAccountRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// One of `viewer` | `editor` | `admin`. `owner` is rejected with 400.
    pub role: String,
}

/// Request body for `PATCH .../service-accounts/{sa_id}`. Both fields optional —
/// rename and/or toggle the disabled state.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PatchServiceAccountRequest {
    #[serde(default)]
    pub name: Option<String>,
    /// `true` ⇒ set `disabled_at = now()`; `false` ⇒ clear it (re-enable).
    #[serde(default)]
    pub disabled: Option<bool>,
}

/// Request body for `POST .../service-accounts/{sa_id}/tokens`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateServiceAccountTokenRequest {
    pub name: String,
    /// Optional RFC 3339 expiry. `None` = never expires.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
}

/// Response for a freshly-minted service-account token. `secret` is the full
/// `sat_{id}.{secret}` credential, returned ONCE and never stored in plaintext.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreatedServiceAccountToken {
    pub id: Uuid,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// The full `sat_{id}.{secret}` token — shown exactly once.
    pub secret: String,
}
