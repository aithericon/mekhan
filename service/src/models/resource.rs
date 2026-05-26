//! Resource DB row structs + wire DTOs.
//!
//! Phase B.5 added the DB row shapes (`ResourceRow`, `ResourceVersionRow`)
//! the resolver reads. Phase B.9 adds the CRUD wire DTOs the handlers
//! deserialize and serialize.
//!
//! These structs deliberately mirror the migration column order (see
//! `service/migrations/20240120000000_create_resources.sql`) so a `SELECT *`
//! reads back via `sqlx::FromRow` without surprises.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// One row from the `resources` table. The "logical" resource — what bumps
/// version on rotation, what soft-delete tombstones. Per-version data
/// (Vault path, public config) lives in [`ResourceVersionRow`].
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ResourceRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub path: String,
    pub resource_type: String,
    pub display_name: String,
    pub latest_version: i32,
    /// `Some(_)` means soft-deleted; the resolver refuses to resolve
    /// tombstoned resources even when a pinned instance points at one.
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One row from the `resource_versions` table. Immutable once written;
/// rotation inserts a new row at `version = latest_version + 1` rather than
/// mutating in place. Pinned instances continue to resolve against their
/// captured version.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ResourceVersionRow {
    pub resource_id: Uuid,
    pub version: i32,
    /// Deterministic Vault path. The resolver composes
    /// `{{secret:<vault_path>#<field>}}` for each secret field of the type.
    pub vault_path: String,
    /// `{ field_name -> json_value }` keyed by names from
    /// `ResourceTypeDescriptor.public_fields`.
    pub public_config: serde_json::Value,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

// ── Wire DTOs (Phase B.9) ─────────────────────────────────────────────────

/// Compact list-row shape. Returned by `GET /api/resources` — never carries
/// per-version data (`public_config`, `vault_path`) so the list endpoint
/// stays cheap to render.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResourceSummary {
    pub id: Uuid,
    pub path: String,
    pub resource_type: String,
    pub display_name: String,
    pub latest_version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// User-supplied key names for dynamic-fields resources (`kv`). The
    /// picker uses this list to emit `<path>.<key>` entries; resolver
    /// uses it to build the secret-template envelope. `None` for typed
    /// resources whose fields are static on the descriptor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_keys: Option<Vec<String>>,
}

impl From<ResourceRow> for ResourceSummary {
    fn from(r: ResourceRow) -> Self {
        Self {
            id: r.id,
            path: r.path,
            resource_type: r.resource_type,
            display_name: r.display_name,
            latest_version: r.latest_version,
            created_at: r.created_at,
            updated_at: r.updated_at,
            dynamic_keys: None,
        }
    }
}

/// Admin view returned by `GET /api/resources/{id}`. Secret fields appear
/// in `redacted_secret_fields` so the picker can render "<redacted>"
/// placeholders without ever shipping the real values.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResourceDetail {
    pub id: Uuid,
    pub path: String,
    pub resource_type: String,
    pub display_name: String,
    pub latest_version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Public fields of the latest version, inline. Same shape the resolver
    /// would assemble (minus the secret-template refs).
    pub public_config: serde_json::Value,
    /// Names of fields the type marks as secret. The frontend renders these
    /// as redacted inputs; the real values live in Vault only.
    pub redacted_secret_fields: Vec<String>,
}

/// One descriptor surfaced by `GET /api/resources/types`. Drives the
/// picker's type list and the schema-driven create form. `schema` is the
/// schemars JSON Schema of the underlying ResourceType struct; the frontend
/// renders it field-by-field.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResourceTypeInfo {
    pub name: String,
    pub display_name: String,
    pub icon: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_provider: Option<String>,
    pub secret_fields: Vec<String>,
    pub public_fields: Vec<String>,
    /// JSON Schema of the type. Cached at first request, then reused.
    pub schema: serde_json::Value,
    /// `true` for the `kv` escape hatch: the field set is per-INSTANCE,
    /// so `secret_fields` / `public_fields` are empty at the type level
    /// and the picker drives off `ResourceSummary.dynamic_keys`. Types
    /// derived via `#[derive(ResourceType)]` always set this to `false`.
    #[serde(default)]
    pub dynamic_fields: bool,
}

/// Request body for `POST /api/resources`. Carries every field needed to
/// land both a `resources` row and the first `resource_versions` row.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateResourceRequest {
    /// Windmill-style path identifier: `^[ufg]/[a-z0-9_-]+/[a-z0-9_-]+$`.
    pub path: String,
    /// Wire identifier from `ResourceTypeDescriptor.name`.
    pub resource_type: String,
    /// UI label. Defaults to `path` if empty.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Full config map — both public and secret fields. The handler splits
    /// it by descriptor lists.
    pub config: serde_json::Value,
    /// Optional workspace scoping. No `workspaces` table exists in v1; a
    /// `None` here resolves to `Uuid::nil()`. When the table lands, this
    /// will be set by the auth layer.
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
}

/// Request body for `PUT /api/resources/{id}`. Either `display_name` or
/// `config` (or both) may be set; if `config` is set the call bumps
/// `latest_version` and writes a new vault_path. `display_name`-only
/// updates do **not** bump version.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateResourceRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub config: Option<serde_json::Value>,
}

/// Request body for `POST /api/resources/{id}/rotate`. Always bumps
/// version. The body carries the new config — the type cannot change at
/// rotation time (`resource_type` is immutable for a logical resource).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RotateResourceRequest {
    pub config: serde_json::Value,
}

/// One row from `resource_audit`. Returned by `GET /api/resources/{id}/audit`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct ResourceAuditEntry {
    pub id: i64,
    pub resource_id: Uuid,
    pub resource_version: i32,
    pub action: String,
    pub principal_id: Uuid,
    pub site: String,
    pub instance_id: Option<Uuid>,
    pub step_id: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

/// Query params for `GET /api/resources`.
#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct ListResourcesQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
    /// Optional filter: only return resources of this type.
    pub resource_type: Option<String>,
    /// Optional workspace filter. v1 default is `Uuid::nil()` so the
    /// no-workspace deployment Just Works; when workspaces land, the auth
    /// layer fills this in.
    pub workspace_id: Option<Uuid>,
}

/// Query params for `GET /api/resources/{id}/audit`.
#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct ListResourceAuditQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
}

fn default_page() -> i64 {
    1
}
fn default_per_page() -> i64 {
    20
}
