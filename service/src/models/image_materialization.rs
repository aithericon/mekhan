//! Image-materialization DB row + wire DTO (docs/22 container staging).
//!
//! Mirrors the `template_stagings` shape in [`crate::models::job_template`]: a
//! per-(container_image version × datacenter) record of pulling an OCI image to
//! an Apptainer `.sif` on that cluster. No Vault coupling — it's a projection of
//! a `materialize_image` effect outcome, not a secret.
//!
//! Column order mirrors `service/migrations/20240136000000_image_materializations.sql`
//! so `SELECT *` reads back via `sqlx::FromRow` without surprises.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// One row from the `image_materializations` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ImageMaterializationRow {
    pub id: Uuid,
    pub container_resource_id: Uuid,
    pub container_version: i32,
    pub datacenter_resource_id: Uuid,
    pub status: String,
    pub digest: Option<String>,
    pub sif_path: Option<String>,
    pub size_bytes: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One materialization row on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ImageMaterialization {
    pub id: Uuid,
    pub container_resource_id: Uuid,
    pub container_version: i32,
    pub datacenter_resource_id: Uuid,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sif_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<ImageMaterializationRow> for ImageMaterialization {
    fn from(r: ImageMaterializationRow) -> Self {
        Self {
            id: r.id,
            container_resource_id: r.container_resource_id,
            container_version: r.container_version,
            datacenter_resource_id: r.datacenter_resource_id,
            status: r.status,
            digest: r.digest,
            sif_path: r.sif_path,
            size_bytes: r.size_bytes,
            last_error: r.last_error,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}
