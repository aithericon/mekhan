//! DTOs for the embedded access-token (PAT) management endpoints
//! (`/api/auth/tokens`). Each "token" is one Zitadel machine user holding a
//! single Personal Access Token — Zitadel stays the source of truth, so these
//! types carry no validity state, only what the UI renders. The `secret` is
//! surfaced exactly once, in the create response.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request body for `POST /api/auth/tokens`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateTokenRequest {
    /// Human-friendly label — stored as the backing Zitadel machine-user
    /// `name`, shown in the token list.
    pub name: String,
    /// Optional longer note — stored as the machine-user `description`.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional RFC 3339 expiry for the underlying PAT. Omit for no expiry.
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// One token row in `GET /api/auth/tokens`. Never carries the secret.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TokenSummary {
    /// Opaque token id (the backing Zitadel machine-user id). Pass back to
    /// `DELETE /api/auth/tokens/{id}` to revoke.
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// RFC 3339 creation timestamp, when Zitadel reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// RFC 3339 PAT expiry — best-effort (omitted if Zitadel doesn't report it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Response of `POST /api/auth/tokens`. Identical to [`TokenSummary`] plus the
/// one-time `secret` — Mekhan never stores or re-serves it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CreatedToken {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// The Personal Access Token. Present only here, only once.
    pub secret: String,
}
