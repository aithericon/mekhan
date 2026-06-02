//! Job-template DB row structs + wire DTOs (Phase 3, B-model).
//!
//! A job template is a reusable, flavor-tagged (`slurm` | `nomad`) cluster job
//! spec authored once in the control plane and staged onto N datacenter
//! resources. The versioning + soft-delete + workspace-scope shape mirrors
//! [`crate::models::resource`] deliberately — the one load-bearing difference
//! is that a job template carries NO Vault coupling. It's a spec, not a secret,
//! so the per-version payload (`common_spec` / `escape_hatch` / `parameters`)
//! lives inline as JSONB rather than behind a `vault_path`.
//!
//! These structs mirror the migration column order (see
//! `service/migrations/20240133000000_job_templates.sql`) so a `SELECT *` reads
//! back via `sqlx::FromRow` without surprises.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use utoipa::ToSchema;
use uuid::Uuid;

// ── DB rows ────────────────────────────────────────────────────────────────

/// One row from the `job_templates` table — the logical template. Per-version
/// payload lives in [`JobTemplateVersionRow`].
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobTemplateRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub flavor: String,
    pub visibility: String,
    pub consumer_locked: bool,
    pub latest_version: i32,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// `Some(_)` means soft-deleted.
    pub deleted_at: Option<DateTime<Utc>>,
}

/// One row from the `job_template_versions` table. Immutable once written; a
/// spec/escape_hatch/parameters change inserts a new row at
/// `version = latest_version + 1` rather than mutating in place.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct JobTemplateVersionRow {
    pub template_id: Uuid,
    pub version: i32,
    /// Typed flavor-neutral core — see [`CommonSpec`].
    pub common_spec: serde_json::Value,
    /// Flavor-specific raw passthrough — see [`EscapeHatch`]. NULL when unused.
    pub escape_hatch: Option<serde_json::Value>,
    /// Declared parameters — see [`TemplateParameter`].
    pub parameters: serde_json::Value,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One row from the `template_stagings` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TemplateStagingRow {
    pub id: Uuid,
    pub template_id: Uuid,
    pub template_version: i32,
    pub datacenter_resource_id: Uuid,
    pub status: String,
    pub remote_ref: Option<String>,
    pub staged_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ── JSONB payload shapes ────────────────────────────────────────────────────

/// Typed flavor-neutral core of a job template version. Every field is optional
/// — a template may specify as much or as little as it likes; the flavor's
/// staging step fills in defaults. Serialized into `job_template_versions.common_spec`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CommonSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpus: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_mb: Option<i64>,
    /// Walltime string in the flavor's own grammar (e.g. Slurm `"01:30:00"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_limit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

/// Flavor-specific raw passthrough. Slurm fills `sbatch_directives`; Nomad fills
/// `hcl_stanza`. Serialized into `job_template_versions.escape_hatch`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct EscapeHatch {
    /// Raw `#SBATCH` directive lines (slurm flavor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sbatch_directives: Vec<String>,
    /// Raw HCL job stanza (nomad flavor).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hcl_stanza: Option<String>,
}

/// One declared parameter the template exposes to its consumers. Serialized as
/// an element of the `job_template_versions.parameters` array.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TemplateParameter {
    pub name: String,
    /// Free-form kind tag (`string` | `int` | `bool` | …). Kept a string so the
    /// vocabulary can grow without an ALTER.
    pub kind: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ── Wire DTOs ───────────────────────────────────────────────────────────────

/// Compact list-row shape. Returned by `GET /api/v1/job-templates` — never
/// carries per-version payload so the list endpoint stays cheap.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JobTemplateSummary {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub flavor: String,
    pub visibility: String,
    pub consumer_locked: bool,
    pub latest_version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<JobTemplateRow> for JobTemplateSummary {
    fn from(r: JobTemplateRow) -> Self {
        Self {
            id: r.id,
            slug: r.slug,
            display_name: r.display_name,
            flavor: r.flavor,
            visibility: r.visibility,
            consumer_locked: r.consumer_locked,
            latest_version: r.latest_version,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// One version, materialized with its decoded JSONB payload. Member of
/// [`JobTemplateDetail::versions`].
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JobTemplateVersion {
    pub version: i32,
    pub common_spec: CommonSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escape_hatch: Option<EscapeHatch>,
    pub parameters: Vec<TemplateParameter>,
    pub created_at: DateTime<Utc>,
}

/// One staging row, on the wire. Member of [`JobTemplateDetail::stagings`] and
/// the body of `GET /api/v1/job-templates/{id}/stagings`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TemplateStaging {
    pub id: Uuid,
    pub template_id: Uuid,
    pub template_version: i32,
    pub datacenter_resource_id: Uuid,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<TemplateStagingRow> for TemplateStaging {
    fn from(r: TemplateStagingRow) -> Self {
        Self {
            id: r.id,
            template_id: r.template_id,
            template_version: r.template_version,
            datacenter_resource_id: r.datacenter_resource_id,
            status: r.status,
            remote_ref: r.remote_ref,
            staged_at: r.staged_at,
            last_error: r.last_error,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

/// Detail view returned by `GET /api/v1/job-templates/{id}`: the template plus
/// its full version history and current stagings.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct JobTemplateDetail {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
    pub flavor: String,
    pub visibility: String,
    pub consumer_locked: bool,
    pub latest_version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// All versions, newest first.
    pub versions: Vec<JobTemplateVersion>,
    /// Current stagings across every datacenter.
    pub stagings: Vec<TemplateStaging>,
}

// ── Request bodies ──────────────────────────────────────────────────────────

/// Request body for `POST /api/v1/job-templates`. Lands a `job_templates` row
/// at `latest_version = 1` plus the first `job_template_versions` row (v1).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreateJobTemplateRequest {
    /// Identifier-safe key, unique within a workspace.
    pub slug: String,
    pub display_name: String,
    /// `slurm` | `nomad`.
    pub flavor: String,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub consumer_locked: Option<bool>,
    pub common_spec: CommonSpec,
    #[serde(default)]
    pub escape_hatch: Option<EscapeHatch>,
    #[serde(default)]
    pub parameters: Option<Vec<TemplateParameter>>,
    /// Optional workspace scoping. `None` resolves to the caller's workspace.
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
}

/// Request body for `PUT /api/v1/job-templates/{id}`. A change to any of
/// `common_spec` / `escape_hatch` / `parameters` BUMPS a new version;
/// metadata-only changes (`display_name` / `visibility` / `consumer_locked`)
/// do not.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdateJobTemplateRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub consumer_locked: Option<bool>,
    #[serde(default)]
    pub common_spec: Option<CommonSpec>,
    #[serde(default)]
    pub escape_hatch: Option<EscapeHatch>,
    #[serde(default)]
    pub parameters: Option<Vec<TemplateParameter>>,
}

// ── Query params ────────────────────────────────────────────────────────────

/// Query params for `GET /api/v1/job-templates`.
#[derive(Debug, Deserialize, ToSchema, utoipa::IntoParams)]
pub struct ListJobTemplatesQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
    /// Optional filter: only return templates of this flavor (`slurm` | `nomad`).
    pub flavor: Option<String>,
    /// Optional workspace filter. Defaults to the caller's workspace.
    pub workspace_id: Option<Uuid>,
}

fn default_page() -> i64 {
    1
}
fn default_per_page() -> i64 {
    20
}
