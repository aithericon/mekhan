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
    /// `subject_as_uuid()` of whoever last mutated the resource (Phase 2).
    /// Backfilled to `created_by` for pre-migration rows.
    pub updated_by: Option<Uuid>,
    /// Polymorphic owner kind (docs/20 §2). Backfilled to `'workspace'` for
    /// existing rows; `scope_id` then equals `workspace_id`.
    #[serde(default)]
    pub scope_kind: String,
    /// Polymorphic owner id. Backfilled = `workspace_id` for legacy rows.
    #[serde(default)]
    pub scope_id: Option<Uuid>,
    /// Virtual folder prefix (docs/20 §3). UI grouping only — never part of
    /// the ref-key (`path`).
    #[serde(default)]
    pub display_path: Option<String>,
    /// Privacy opt-out: `true` removes the workspace-role floor so access comes
    /// solely from grants + inheritance (see `auth/grants.rs`). `#[sqlx(default)]`
    /// so explicit-column SELECTs that don't list it still map.
    #[serde(default)]
    #[sqlx(default)]
    pub restricted: bool,
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

/// Compact list-row shape. Returned by `GET /api/v1/resources` — never carries
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
    /// Creator (`subject_as_uuid()`), resolvable via `users`.
    pub created_by: Uuid,
    /// Last mutator (`subject_as_uuid()`). NULL for pre-Phase-2 rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
    /// User-supplied key names for dynamic-fields resources (`kv`). The
    /// picker uses this list to emit `<path>.<key>` entries; resolver
    /// uses it to build the secret-template envelope. `None` for typed
    /// resources whose fields are static on the descriptor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dynamic_keys: Option<Vec<String>>,
    /// The latest version's public config — populated ONLY for `capacity`
    /// rows, so the editor's deployment picker can discriminate a capacity by
    /// its `liveness` axis (presence → runner group, seeded → concurrency
    /// limit, competing_consumer → worker) without an N+1 detail fetch per
    /// alias. `None` for every other kind: the list endpoint otherwise stays
    /// cheap and never carries per-version data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_config: Option<serde_json::Value>,
    /// The caller's effective object role on this resource (folder cascade +
    /// override + grants, ws floor unless `restricted`). Drives the editor's
    /// edit/share gating. NOT a DB column — stamped by the handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_effective_role: Option<String>,
    /// Privacy opt-out (no workspace-role floor — access via grants only).
    #[serde(default)]
    pub restricted: bool,
}

impl crate::auth::AclAnnotated for ResourceSummary {
    fn acl_id(&self) -> Uuid {
        self.id
    }
    fn set_my_effective_role(&mut self, role: Option<String>) {
        self.my_effective_role = role;
    }
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
            created_by: r.created_by,
            updated_by: r.updated_by,
            dynamic_keys: None,
            public_config: None,
            my_effective_role: None,
            restricted: r.restricted,
        }
    }
}

/// Admin view returned by `GET /api/v1/resources/{id}`. Secret fields appear
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
    /// Creator (`subject_as_uuid()`), resolvable via `users`.
    pub created_by: Uuid,
    /// Last mutator (`subject_as_uuid()`). NULL for pre-Phase-2 rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
    /// Public fields of the latest version, inline. Same shape the resolver
    /// would assemble (minus the secret-template refs).
    pub public_config: serde_json::Value,
    /// Names of fields the type marks as secret. The frontend renders these
    /// as redacted inputs; the real values live in Vault only.
    pub redacted_secret_fields: Vec<String>,
    /// The caller's effective object role on this resource — drives edit/share
    /// gating. NOT a DB column — stamped by the handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_effective_role: Option<String>,
    /// Privacy opt-out (no workspace-role floor — access via grants only).
    #[serde(default)]
    pub restricted: bool,
    /// Owner scope kind (`workspace` | `folder` | `template`) — the placement /
    /// inheritance parent (docs/20 §2). Drives the edit sheet's move control.
    pub scope_kind: String,
    /// Owner scope id. For `workspace`, the workspace id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<Uuid>,
}

/// One descriptor surfaced by `GET /api/v1/resources/types`. Drives the
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
    /// Named trait-space presets (doc 23 §7) the create form can prefill —
    /// populated ONLY for the `capacity` type (`Some([worker, instrument,
    /// hpc])`), `None` for every other kind. A create call names a preset via
    /// the `preset` config key to get the locked axes, overriding only the free
    /// ones. See `models::capacity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_presets: Option<Vec<crate::models::capacity::CapacityPreset>>,
}

/// Request body for `POST /api/v1/resources`. Carries every field needed to
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
    /// Placement scope (docs/20 §2): `workspace` (default), `folder`, or
    /// `template`. Folder/template placement makes the resource non-
    /// workspace-wide and is the inheritance parent for the object ACL.
    #[serde(default)]
    pub scope_kind: Option<String>,
    /// Owner id for a `folder`/`template` scope. Ignored for `workspace`.
    #[serde(default)]
    pub scope_id: Option<Uuid>,
    /// Create the resource `restricted` (private — no workspace-role floor).
    #[serde(default)]
    pub restricted: Option<bool>,
}

/// Request body for `PUT /api/v1/resources/{id}`. Either `display_name` or
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

/// Request body for `POST /api/v1/resources/{id}/rotate`. Always bumps
/// version. The body carries the new config — the type cannot change at
/// rotation time (`resource_type` is immutable for a logical resource).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RotateResourceRequest {
    pub config: serde_json::Value,
}

/// Outcome of `POST /api/v1/resources/{id}/repair` — operator recovery for a
/// pool whose backing net was lost or drifted. Reports the deterministic pool
/// net id that was (re)deployed and how many live presence sources were re-armed
/// to re-acquire their capacity on their next heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RepairPoolResponse {
    /// The backing pool net id (`pool-<resource_id>`) that was re-ensured.
    pub pool_net_id: String,
    /// Whether the resource resolves to a backing pool net at all (a non-pool
    /// resource is a no-op repair). `false` means nothing was redeployed.
    pub has_pool_net: bool,
    /// Number of present runners (machine pools) re-armed to re-acquire their
    /// capacity tokens on their next heartbeat.
    pub runners_rearmed: usize,
    /// Number of present roster members (human pools) re-armed to re-acquire on
    /// their next presence heartbeat.
    pub members_rearmed: usize,
    /// Number of stale held leases reclaimed — `in_use` tokens whose holder
    /// instance was terminal (completed/failed/cancelled) or gone, released back
    /// to the pool via the net's own `t_release`. Live leases (holder still
    /// running) are never reclaimed.
    pub leases_reclaimed: usize,
}

/// One row from `resource_audit`. Returned by `GET /api/v1/resources/{id}/audit`.
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

/// Query params for `GET /api/v1/resources`.
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
    /// Scope context for downward-visibility resolution (docs/20 §2). Format:
    /// `workspace`, `folder:<uuid>`, or `template:<uuid>`. When present it
    /// overrides `workspace_id`: the list returns the most-specific-wins
    /// visible set for the binding context. When absent, the legacy flat
    /// `workspace_id` filter applies.
    pub scope: Option<String>,
    /// Optional virtual-folder prefix filter on `display_path` (docs/20 §3).
    pub folder: Option<String>,
    /// When `true` (with `scope`), return only resources owned by EXACTLY that
    /// scope (placement filter), not the downward-visible most-specific-wins
    /// set. The management browser uses this so a folder shows what is *placed
    /// in* it and the workspace root shows only workspace-scoped resources.
    #[serde(default)]
    pub exact: Option<bool>,
}

/// Query params for `GET /api/v1/resources/{id}/audit`.
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
