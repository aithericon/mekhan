//! Asset DB row structs + wire DTOs (docs/20 §4/§7/§8).
//!
//! The asset layer is **user-typed, curated, static content** — material
//! parameters, simulation scripts, reference artifacts — stored as
//! schema-validated JSONB rows (+ S3 for `File` fields) and consumed by
//! workflow nodes as ordinary staged inputs. It is a *separate* layer from
//! resources (credentials) and the catalogue (machine-produced outputs).
//!
//! These structs deliberately mirror the migration column order (see
//! `service/migrations/20240133000000_create_assets.sql`) so a `SELECT *`
//! reads back via `sqlx::FromRow` without surprises — exactly like
//! [`crate::models::resource`], which this module mirrors.
//!
//! Field schemas reuse [`PortField`] wholesale (docs/20 §4.1) — there is no
//! asset-specific field vocabulary. Records validate against
//! `Port::json_schema` / `FieldKind::json_schema` like ports do.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::models::template::PortField;

// ── Scope (docs/20 §2) ────────────────────────────────────────────────────

/// Polymorphic owner discriminator. A resource/asset/asset-type is owned by
/// **exactly one** scope; visibility flows downward (template ⊃ folder ⊃
/// workspace) with most-specific-wins. See [`crate::scope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Workspace,
    Folder,
    Template,
}

impl ScopeKind {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Folder => "folder",
            Self::Template => "template",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "workspace" => Some(Self::Workspace),
            "folder" => Some(Self::Folder),
            "template" => Some(Self::Template),
            _ => None,
        }
    }
}

/// Cardinality of an asset type (docs/20 §4.2). `Object` is the 1-row
/// degenerate case (the builder renders a single-row form instead of a grid);
/// `Collection` is a many-row table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    Object,
    Collection,
}

impl Cardinality {
    pub fn as_db(&self) -> &'static str {
        match self {
            Self::Object => "object",
            Self::Collection => "collection",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "object" => Some(Self::Object),
            "collection" => Some(Self::Collection),
            _ => None,
        }
    }
}

// ── DB row structs (mirror migration column order) ─────────────────────────

/// One row from the `asset_types` table. The user-defined schema: an ordered
/// list of [`PortField`]s, scoped + foldered.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AssetTypeRow {
    pub id: Uuid,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub name: String,
    pub display_name: String,
    pub display_path: Option<String>,
    /// `Vec<PortField>` stored as JSONB. Read back as raw JSON; deserialize to
    /// `Vec<PortField>` at the handler edge.
    pub fields_json: serde_json::Value,
    pub cardinality: String,
    pub version: i32,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// Last mutator (`subject_as_uuid()`). NULL for pre-Phase-2 rows.
    pub updated_by: Option<Uuid>,
}

/// One row from the `assets` table. A named, version-pinned, scope-owned
/// collection of records of one [`AssetTypeRow`]. Records live in
/// [`AssetRecordRow`].
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AssetRow {
    pub id: Uuid,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub type_id: Uuid,
    pub ref_key: String,
    pub display_name: String,
    pub display_path: Option<String>,
    pub version: i32,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// Last mutator (`subject_as_uuid()`). NULL for pre-Phase-2 rows.
    pub updated_by: Option<Uuid>,
    /// Privacy opt-out (no workspace-role floor — access via grants only).
    /// `#[sqlx(default)]` so explicit-column SELECTs that omit it still map.
    #[serde(default)]
    #[sqlx(default)]
    pub restricted: bool,
}

/// One row from the `asset_records` table. A schema-validated JSONB row,
/// versioned with the asset. `File` fields store an S3 storage path inside
/// `data`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AssetRecordRow {
    pub asset_id: Uuid,
    pub version: i32,
    pub row_idx: i32,
    pub data: serde_json::Value,
}

// ── Wire DTOs — asset types ────────────────────────────────────────────────

/// Compact list-row for `GET /api/v1/asset-types` — omits `fields_json` so the
/// list stays cheap.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AssetTypeSummary {
    pub id: Uuid,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub name: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_path: Option<String>,
    pub cardinality: String,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Creator (`subject_as_uuid()`), resolvable via `user_profiles`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
    /// Last mutator (`subject_as_uuid()`). NULL for pre-Phase-2 rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
}

impl From<AssetTypeRow> for AssetTypeSummary {
    fn from(r: AssetTypeRow) -> Self {
        Self {
            id: r.id,
            scope_kind: r.scope_kind,
            scope_id: r.scope_id,
            name: r.name,
            display_name: r.display_name,
            display_path: r.display_path,
            cardinality: r.cardinality,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
            created_by: r.created_by,
            updated_by: r.updated_by,
        }
    }
}

/// Full view for `GET /api/v1/asset-types/{id}` — carries the schema.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AssetTypeDetail {
    pub id: Uuid,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub name: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_path: Option<String>,
    /// The schema: an ordered list of [`PortField`]s.
    pub fields: Vec<PortField>,
    pub cardinality: String,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Creator (`subject_as_uuid()`), resolvable via `user_profiles`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
    /// Last mutator (`subject_as_uuid()`). NULL for pre-Phase-2 rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
}

/// Request body for `POST /api/v1/asset-types`. Validates the schema +
/// ident-grammar `name`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateAssetTypeRequest {
    /// Flat identifier ref-key, `^[a-z][a-z0-9_]*$`.
    pub name: String,
    pub display_name: Option<String>,
    /// Virtual folder prefix (e.g. `materials/metals`).
    #[serde(default)]
    pub display_path: Option<String>,
    /// The schema — an ordered list of [`PortField`]s.
    pub fields: Vec<PortField>,
    /// `object` | `collection`. Defaults to `collection`.
    #[serde(default = "default_cardinality")]
    pub cardinality: Cardinality,
    /// Owner scope. Defaults to `workspace` of the caller when omitted.
    #[serde(default)]
    pub scope_kind: Option<ScopeKind>,
    /// Owner scope id. For `workspace`, defaults to the caller's workspace.
    #[serde(default)]
    pub scope_id: Option<Uuid>,
}

/// Request body for `PUT /api/v1/asset-types/{id}`. Schema updates must be
/// **additive-only** (docs/20 §4.3): add optional fields or widen; rename /
/// remove / retype / newly-require is rejected server-side.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateAssetTypeRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub display_path: Option<String>,
    /// New schema. When present it is validated additive-only against the
    /// current schema and bumps `version`.
    #[serde(default)]
    pub fields: Option<Vec<PortField>>,
}

// ── Wire DTOs — assets ─────────────────────────────────────────────────────

/// Compact list-row for `GET /api/v1/assets`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AssetSummary {
    pub id: Uuid,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub type_id: Uuid,
    pub ref_key: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_path: Option<String>,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Creator (`subject_as_uuid()`), resolvable via `user_profiles`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
    /// Last mutator (`subject_as_uuid()`). NULL for pre-Phase-2 rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
    /// The caller's effective object role on this asset — drives edit/share
    /// gating. NOT a DB column — stamped by the handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_effective_role: Option<String>,
    /// Privacy opt-out (no workspace-role floor — access via grants only).
    #[serde(default)]
    pub restricted: bool,
}

impl From<AssetRow> for AssetSummary {
    fn from(r: AssetRow) -> Self {
        Self {
            id: r.id,
            scope_kind: r.scope_kind,
            scope_id: r.scope_id,
            type_id: r.type_id,
            ref_key: r.ref_key,
            display_name: r.display_name,
            display_path: r.display_path,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
            created_by: r.created_by,
            updated_by: r.updated_by,
            my_effective_role: None,
            restricted: r.restricted,
        }
    }
}

/// Full view for `GET /api/v1/assets/{id}` — metadata + a page of records.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AssetDetail {
    pub id: Uuid,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub type_id: Uuid,
    pub ref_key: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_path: Option<String>,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Creator (`subject_as_uuid()`), resolvable via `user_profiles`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
    /// Last mutator (`subject_as_uuid()`). NULL for pre-Phase-2 rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
    /// The current-version records (paged). Each is a validated JSONB row.
    pub records: Vec<serde_json::Value>,
    /// Total record count for the current version (for pagination).
    pub record_count: i64,
    /// The caller's effective object role on this asset — drives edit/share
    /// gating. NOT a DB column — stamped by the handler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_effective_role: Option<String>,
    /// Privacy opt-out (no workspace-role floor — access via grants only).
    #[serde(default)]
    pub restricted: bool,
}

/// Request body for `POST /api/v1/assets`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateAssetRequest {
    pub type_id: Uuid,
    /// Flat identifier, `^[a-z][a-z0-9_]*$`.
    pub ref_key: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub display_path: Option<String>,
    #[serde(default)]
    pub scope_kind: Option<ScopeKind>,
    #[serde(default)]
    pub scope_id: Option<Uuid>,
    /// Create the asset `restricted` (private — no workspace-role floor).
    #[serde(default)]
    pub restricted: Option<bool>,
}

/// Request body for `PUT /api/v1/assets/{id}/records`. Replaces (or appends to)
/// the record set; bumps `version` and validates each row against the type's
/// schema.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ReplaceRecordsRequest {
    /// The new record rows. Each is validated against the asset type's
    /// `fields_json` via `Port::json_schema`.
    pub records: Vec<serde_json::Value>,
    /// When `true`, append to the current version's records instead of
    /// replacing them. Default `false` = replace.
    #[serde(default)]
    pub append: bool,
}

/// Multipart-form field-mapping params for `POST /api/v1/assets/{id}/import-csv`.
/// The CSV column headers map to asset-type field `name`s; unmapped columns are
/// ignored. Sent as query params alongside the multipart file body.
#[derive(Debug, Clone, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct ImportCsvParams {
    /// When `true`, the first CSV row is treated as a header row whose cells are
    /// field names. When `false`, columns map positionally to the type's field
    /// order. Default `true`.
    #[serde(default = "default_true")]
    pub has_header: bool,
    /// When `true`, append the imported rows to the current version; default
    /// `false` = replace.
    #[serde(default)]
    pub append: bool,
}

// ── Query params ───────────────────────────────────────────────────────────

/// Query params for the asset/asset-type **create** endpoints. Lets callers
/// specify the owner scope the same way the list endpoints do (`?scope=`),
/// instead of (or in agreement with) the body's `scope_kind`/`scope_id`. When
/// both are given they must agree; conflict → 400.
#[derive(Debug, Default, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct CreateScopeQuery {
    /// Owner scope. Format: `workspace`, `folder:<uuid>`, or `template:<uuid>`.
    /// Omitted → falls back to the body scope, then the caller's workspace.
    pub scope: Option<String>,
}

/// Query params for `GET /api/v1/asset-types`.
#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct ListAssetTypesQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
    /// Scope context for downward-visibility resolution. `workspace` (the
    /// caller's workspace) when omitted. Format: `workspace`, `folder:<uuid>`,
    /// or `template:<uuid>`.
    pub scope: Option<String>,
    /// Optional virtual-folder prefix filter on `display_path`.
    pub folder: Option<String>,
}

/// Query params for `GET /api/v1/assets`.
#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct ListAssetsQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
    /// Only assets of this type.
    pub type_id: Option<Uuid>,
    /// Scope context for downward-visibility resolution (see
    /// [`ListAssetTypesQuery::scope`]).
    pub scope: Option<String>,
    /// Optional virtual-folder prefix filter on `display_path`.
    pub folder: Option<String>,
}

/// Query params for `GET /api/v1/assets/{id}` record pagination.
#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct GetAssetQuery {
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
fn default_cardinality() -> Cardinality {
    Cardinality::Collection
}
fn default_true() -> bool {
    true
}

/// One run (workflow instance) that pinned a given asset — a reverse-lineage
/// row for `GET /api/v1/assets/{id}/usage` (docs/20 §9). `alias` /
/// `version_used` are extracted from the instance's `asset_pins` map for the
/// queried asset. This is **asset-level** lineage ("runs that used asset X");
/// record/material-level lineage ("runs that used Copper C110") is deferred —
/// see docs/20 §9.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AssetUsageItem {
    pub instance_id: Uuid,
    pub template_id: Uuid,
    pub template_name: String,
    pub template_version: i32,
    pub status: String,
    pub mode: String,
    /// The binding alias under which this run consumed the asset.
    pub alias: String,
    /// The asset version this run pinned (immutable for the life of the run).
    pub version_used: i32,
    pub created_at: DateTime<Utc>,
}

/// Pagination for `GET /api/v1/assets/{id}/usage`.
#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct AssetUsageQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
}
