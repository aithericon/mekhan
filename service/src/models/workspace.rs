//! Wire DTOs for the workspaces, folders, tags, and visibility surface.
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
    /// Human-readable identity, LEFT JOINed from `user_profiles` (populated by
    /// the auth extractor on each authenticated request). `None` for a member
    /// who was added by `subject` but has never logged into mekhan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Profile photo URL, LEFT JOINed from `user_profiles.avatar_url`. `None`
    /// when the member has no profile row or no `picture` claim → SPA initials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
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

/// A folder node in a workspace's single-parent template tree (filesystem
/// model). `path` is the materialized path ('/a/b/c'); the frontend builds the
/// tree from `parent_id`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, sqlx::FromRow)]
pub struct Folder {
    pub id: Uuid,
    pub workspace_id: Uuid,
    /// Parent folder, or `None` for a root-level folder.
    pub parent_id: Option<Uuid>,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    /// Materialized path, e.g. `/research/q3`. Unique within a workspace.
    pub path: String,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
    /// Advanced on rename/move (Phase 2). DEFAULT NOW() at row birth.
    pub updated_at: DateTime<Utc>,
    /// Last mutator (`subject_as_uuid()`). Backfilled to `created_by`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateFolderRequest {
    /// Parent folder; `None` creates a root-level folder.
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    pub slug: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
}

/// Partial update for a folder. All fields optional. Supplying `slug` and/or
/// `parent_id` performs a MOVE (subtree paths are rewritten); `display_name` /
/// `description` are COALESCE renames.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateFolderRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// New parent folder (move). Present-and-`null` is ambiguous with absent in
    /// flat JSON, so a move-to-root is expressed via `slug` change or by the
    /// caller setting a different parent; `Some(id)` reparents under `id`.
    #[serde(default)]
    pub parent_id: Option<Uuid>,
    #[serde(default)]
    pub slug: Option<String>,
}

/// Set (or clear) the home folder of a template. `None` moves the template to
/// the workspace root (deletes its `template_folders` row).
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetFolderRequest {
    #[serde(default)]
    pub folder_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTagsRequest {
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetVisibilityRequest {
    /// `workspace` (default), `public`, or `private`.
    pub visibility: String,
    /// Required when `visibility == "private"`: the owning parent family
    /// (any version id; resolved to its base). Ignored otherwise. The
    /// private sub-workflow may then be embedded only by that family.
    #[serde(default)]
    pub owner_template_id: Option<Uuid>,
}
