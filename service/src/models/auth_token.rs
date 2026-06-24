//! DTOs for the embedded access-token (PAT) management endpoints
//! (`/api/v1/auth/tokens`). Each "token" is one mekhan-native row in the local
//! `user_pats` table — mekhan owns the credential outright (no IdP round-trip),
//! storing only the SHA-256 of the secret half. These types carry only what the
//! UI renders; the `secret` is surfaced exactly once, in the create response.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Request body for `POST /api/v1/auth/tokens`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateTokenRequest {
    /// Human-friendly label — stored as `user_pats.name`, shown in the list.
    pub name: String,
    /// Optional longer note — stored as `user_pats.description`.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional RFC 3339 expiry for the token. Omit for no expiry.
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// One token row in `GET /api/v1/auth/tokens`. Never carries the secret.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TokenSummary {
    /// Opaque token id (the `user_pats.id`). Pass back to
    /// `DELETE /api/v1/auth/tokens/{id}` to revoke.
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// RFC 3339 creation timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// RFC 3339 token expiry — omitted when the token never expires.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Response of `POST /api/v1/auth/tokens`. Identical to [`TokenSummary`] plus the
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
    /// The Personal Access Token (`uat_{id}.{secret}`). Present only here, only
    /// once — mekhan stores only the SHA-256 of the secret half.
    pub secret: String,
}
