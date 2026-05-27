//! Auth domain types — pure, no I/O dependencies.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable namespace used to derive a UUID from an OIDC `sub` claim so the
/// existing `workflow_instances.created_by UUID` column keeps working without
/// a schema migration. A `v5(NAMESPACE, sub)` is deterministic per subject.
pub const SUBJECT_UUID_NAMESPACE: Uuid = Uuid::from_u128(0x6d65_6b68_616e_5f73_756a_6563_745f_7635);

/// An authenticated principal. The domain core works in terms of this type,
/// never in terms of JWTs or provider-specific claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthUser {
    /// OIDC `sub` claim. Stable per identity within an issuer.
    pub subject: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    /// Upstream identity-provider org id (e.g. Zitadel `urn:zitadel:iam:org:id`).
    /// Metadata only — the authoritative tenant is `workspace_id`, looked up
    /// from `workspaces.zitadel_org_id` by `DbPrincipalResolver`.
    pub org_id: Option<String>,
    /// Mekhan workspace the principal is currently acting in. Populated by
    /// `DbPrincipalResolver` from the user's `workspace_members` row (auto-
    /// provisioned from `org_id` when a matching `workspaces.zitadel_org_id`
    /// exists; otherwise the seeded default workspace if the user is a
    /// member there). `None` only when no DB handle is available (unit
    /// tests + legacy session rows; `#[serde(default)]` keeps deserialize
    /// of old session JSON working).
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
}

impl AuthUser {
    /// Deterministic UUID derived from the OIDC subject. Used to populate
    /// pre-existing `UUID NOT NULL` columns (workflow_instances.created_by)
    /// without migrating their type.
    pub fn subject_as_uuid(&self) -> Uuid {
        Uuid::new_v5(&SUBJECT_UUID_NAMESPACE, self.subject.as_bytes())
    }
}

/// JWT claims after the verifier has checked signature, issuer, audience,
/// and expiry. Not yet mapped onto our domain user — that's the resolver's
/// job (provider-specific role claim names live there, not here).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifiedClaims {
    pub subject: String,
    pub issuer: String,
    pub audience: Vec<String>,
    pub expires_at: i64,
    /// All remaining claims, keyed by their JWT name (e.g. `email`, `name`,
    /// `urn:zitadel:iam:org:project:roles`). Resolver picks the ones it
    /// knows about.
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing or malformed Authorization header")]
    MissingToken,
    #[error("invalid token: {0}")]
    InvalidToken(String),
    #[error("token has expired")]
    Expired,
    #[error("issuer does not match expected")]
    IssuerMismatch,
    #[error("audience does not match expected")]
    AudienceMismatch,
    #[error("unable to fetch signing keys: {0}")]
    JwksUnavailable(String),
    #[error("internal auth error: {0}")]
    Internal(String),
}
