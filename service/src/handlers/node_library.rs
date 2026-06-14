//! Library-node catalogue HTTP handler — surfaces published `library_node`
//! templates as palette descriptors. Companion to `/api/v1/node-types` (which
//! lists the built-in *primitives*); this is the "Library" half of the editor
//! palette (decision 6). Unlike the static node-type registry, this list is
//! data-driven (DB rows) and ACL-filtered per caller.

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::error::ApiError;
use crate::models::template::Presentation;
use crate::AppState;

/// A library node as the editor palette consumes it: the stable coordinate,
/// display copy, branding, provenance, lifecycle, and the family + version a
/// drop should pin to. Mirrors the `NodeDescriptor` role for primitives, but
/// carries the embed coordinate instead of a wire name.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryNodeDescriptor {
    /// Stable `vendor/slug` coordinate (decision 7) — the drop stamps this onto
    /// the embedding sub-workflow node as `sourceCoordinate`.
    pub coordinate: String,
    /// Template family id (`COALESCE(base_template_id, id)`) — the value stamped
    /// as the dropped sub-workflow node's `templateId`.
    pub template_id: Uuid,
    /// Current latest version of the family; a drop pins to this version.
    pub version: i32,
    /// Display name.
    pub name: String,
    /// Optional one-line description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Trust axis: `system` (platform-seeded, read-only) | `workspace` |
    /// `community`. Always present for a library node.
    pub origin: String,
    /// Lifecycle: `active` (default) | `deprecated`. `retired` nodes are
    /// excluded from this listing entirely.
    pub lifecycle_status: String,
    /// Branding blob (icon/color/vendor/category/badge), parsed from the row
    /// JSONB. Drives palette grouping (category → vendor) and the frozen card.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<Presentation>,
}

/// Query params for `GET /api/v1/node-library`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LibraryNodeListParams {
    /// Include `deprecated` library nodes (default `false` — only `active`).
    /// `retired` nodes are always excluded; their pinned embeds still resolve
    /// via the version row, but they must not be droppable anew.
    #[serde(default)]
    pub include_deprecated: bool,
}

#[derive(sqlx::FromRow)]
struct LibraryNodeRow {
    coordinate: String,
    template_id: Uuid,
    version: i32,
    name: String,
    description: Option<String>,
    origin: Option<String>,
    lifecycle_status: String,
    presentation: Option<serde_json::Value>,
}

/// GET /api/v1/node-library
///
/// List the library nodes (branded, reusable `sub_workflow` building blocks)
/// the caller may drop onto a canvas. Returns the latest version of each
/// `library_node` family that is visible to the caller (public, or in the
/// caller's workspace) and not `retired`. `deprecated` nodes are excluded
/// unless `include_deprecated=true`. Ordered by category → vendor → name so
/// the palette can render its two-level grouping directly.
#[utoipa::path(
    get,
    path = "/api/v1/node-library",
    params(LibraryNodeListParams),
    responses(
        (status = 200, description = "Visible library nodes", body = Vec<LibraryNodeDescriptor>),
    ),
    tag = "node-library",
)]
pub async fn list_node_library(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<LibraryNodeListParams>,
) -> Result<Json<Vec<LibraryNodeDescriptor>>, ApiError> {
    let workspace_id = user.workspace_id.unwrap_or_else(Uuid::nil);

    // Latest-version library nodes the caller can see. The lifecycle filter
    // keeps the COUNT/SELECT predicate honest: retired always out, deprecated
    // gated behind the flag. Ordering mirrors the palette's grouping so the
    // frontend never has to re-sort.
    let rows = sqlx::query_as::<_, LibraryNodeRow>(
        "SELECT coordinate, \
                COALESCE(base_template_id, id) AS template_id, \
                version, name, description, origin, lifecycle_status, presentation \
           FROM workflow_templates \
          WHERE template_kind = 'library_node' \
            AND is_latest = TRUE \
            AND coordinate IS NOT NULL \
            AND lifecycle_status <> 'retired' \
            AND ($1 OR lifecycle_status = 'active') \
            AND (workspace_id = $2 OR visibility = 'public') \
          ORDER BY (presentation->>'category') NULLS LAST, \
                   (presentation->>'vendor') NULLS LAST, \
                   name",
    )
    .bind(params.include_deprecated)
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await?;

    let items = rows
        .into_iter()
        .map(|r| LibraryNodeDescriptor {
            coordinate: r.coordinate,
            template_id: r.template_id,
            version: r.version,
            name: r.name,
            description: r.description,
            // A library node always carries an origin; default to `system` so a
            // hand-seeded row with a NULL origin still renders sensibly.
            origin: r.origin.unwrap_or_else(|| "system".to_string()),
            lifecycle_status: r.lifecycle_status,
            presentation: r
                .presentation
                .and_then(|v| serde_json::from_value::<Presentation>(v).ok()),
        })
        .collect();

    Ok(Json(items))
}
