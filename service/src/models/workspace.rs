//! Wire DTOs for the workspaces, projects, tags, and visibility surface.
//!
//! Workspaces are tenant boundaries created out-of-band (seeded `default`,
//! Zitadel-auto-provisioned, or future admin-spawned). The endpoints here
//! manage *membership* and *grouping* within an existing workspace; they do
//! not create workspaces themselves.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Summary view returned by `GET /workspaces` and embedded in
/// `WorkspaceMember` responses.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct WorkspaceSummary {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub is_system: bool,
    pub created_at: DateTime<Utc>,
}

/// A single `workspace_members` row. `user_id` is derived from the OIDC
/// subject via `uuid_v5(SUBJECT_UUID_NAMESPACE, subject)` — the same
/// derivation the resolver uses.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct WorkspaceMember {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AddMemberRequest {
    /// OIDC `sub` claim — the server derives `user_id` via
    /// `uuid_v5(SUBJECT_UUID_NAMESPACE, subject)`. Phase B will add an
    /// email→subject resolver for the admin UI.
    pub subject: String,
    /// One of: `owner`, `admin`, `editor`, `viewer`.
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateProjectRequest {
    pub slug: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AttachTemplateRequest {
    /// The *base* template id (the first version's id, which the
    /// `is_latest`-chained version graph hangs off of). Project membership
    /// follows the live version chain so attaching once survives version
    /// bumps.
    pub template_id: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTagsRequest {
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetVisibilityRequest {
    /// `workspace` (default) or `public`.
    pub visibility: String,
}
