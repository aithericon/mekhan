//! Container-image materialization endpoint (docs/22 container staging).
//!
//! One handler under the `container-images` tag. The shape mirrors
//! [`crate::handlers::job_templates::stage_job_template`] — workspace-scoped via
//! the caller's session workspace, kicks a generated one-shot Petri net per
//! target via [`trigger_materialize_image`], returns 202 with the triggered rows
//! — but takes **explicit** targets only (no datacenter auto-enumeration): a
//! materialize is a deliberate per-cluster push, so an empty list is a 400 and
//! any bad target fails the whole request.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::image_materialization::ImageMaterialization;
use crate::petri::staging_net::trigger_materialize_image;
use crate::AppState;

/// Body for `POST /api/v1/container-images/{id}/materialize`.
///
/// Explicit targets only — unlike job-template staging there is no auto-enumerate
/// of every workspace datacenter. An empty list is a 400.
#[derive(Debug, Deserialize, ToSchema)]
pub struct MaterializeRequest {
    /// Datacenter resource ids to materialize the image onto.
    pub datacenter_resource_ids: Vec<Uuid>,
}

/// Caller-implicit workspace: the user's session workspace, or 403 when the
/// caller has no active workspace (no silent nil-tenant fallback).
fn caller_workspace(user: &AuthUser) -> Result<Uuid, ApiError> {
    user.require_workspace()
}

/// `POST /api/v1/container-images/{id}/materialize` — pull a container-image
/// resource version into an Apptainer `.sif` on one or more datacenter clusters.
/// For each target this kicks a generated one-shot materialize Petri net
/// (`materialize_image` effect) and upserts its `image_materializations` row; the
/// rows start at `materializing` and the projection advances them to
/// `ready`/`failed` as the nets complete. Returns 202 with the triggered rows.
///
/// Authority = datacenter-resource access (workspace-scoped), not an admin role:
/// if you can reference cluster X in your workspace, you can materialize onto it.
///
/// Target selection is **explicit only** (differs from job-template staging): an
/// empty `datacenter_resource_ids` is a 400, and the first incompatible /
/// unresolvable target fails the whole request.
#[utoipa::path(
    post,
    path = "/api/v1/container-images/{id}/materialize",
    params(("id" = Uuid, Path, description = "Container image resource id")),
    request_body = MaterializeRequest,
    responses(
        (status = 202, description = "Materialization runs triggered", body = Vec<ImageMaterialization>),
        (status = 400, description = "No targets / incompatible target", body = ErrorResponse),
        (status = 404, description = "Container image not found", body = ErrorResponse),
        (status = 500, description = "Server error", body = ErrorResponse),
    ),
    tag = "container-images",
)]
pub async fn materialize_container_image(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<MaterializeRequest>,
) -> Result<(StatusCode, Json<Vec<ImageMaterialization>>), ApiError> {
    let workspace_id = caller_workspace(&user)?;

    if req.datacenter_resource_ids.is_empty() {
        return Err(ApiError::bad_request(
            "at least one datacenter_resource_id required",
        ));
    }

    let mut out = Vec::with_capacity(req.datacenter_resource_ids.len());
    for dc in req.datacenter_resource_ids {
        match trigger_materialize_image(&state.db, &state.petri, workspace_id, id, dc).await {
            Ok(row) => out.push(ImageMaterialization::from(row)),
            // Targets are explicit, so a bad one is a user error: fail the whole
            // request on the first one (matching stage's explicit-target semantics).
            Err(e) => return Err(ApiError::bad_request(e.to_string())),
        }
    }

    Ok((StatusCode::ACCEPTED, Json(out)))
}
