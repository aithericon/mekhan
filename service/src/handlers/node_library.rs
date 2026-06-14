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
    /// Lifecycle: `active` (default) | `deprecated` | `retired`. `retired`
    /// nodes are excluded from this listing unless `include_retired=true`.
    pub lifecycle_status: String,
    /// Successor coordinate for a `deprecated` node (decision 11) — the palette
    /// shows it as a "use X instead" hint. Absent for `active` nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// Branding blob (icon/color/vendor/category/badge), parsed from the row
    /// JSONB. Drives palette grouping (category → vendor) and the frozen card.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<Presentation>,
    /// The caller's effective workspace/object role label
    /// (`owner|admin|editor|viewer`) on this node's family — annotated by
    /// `list_node_library` so the management view can gate Manage (rebrand +
    /// lifecycle) and Demote to `admin`+. Not a column; the backend still
    /// enforces on every governance mutate path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub my_effective_role: Option<String>,
}

impl crate::auth::AclAnnotated for LibraryNodeDescriptor {
    fn acl_id(&self) -> Uuid {
        // Family-root id (`COALESCE(base_template_id, id)`). `effective_object_roles`
        // collapses the per-version row id to the chain root internally, and the
        // family root *is* that chain root, so keying on it resolves the same
        // role templates.rs resolves from a per-version row id.
        self.template_id
    }
    fn set_my_effective_role(&mut self, role: Option<String>) {
        self.my_effective_role = role;
    }
}

/// Query params for `GET /api/v1/node-library`.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LibraryNodeListParams {
    /// Include `deprecated` library nodes (default `false` — only `active`).
    /// `retired` nodes are excluded unless `include_retired=true`; their pinned
    /// embeds still resolve via the version row, but they must not be droppable
    /// anew.
    #[serde(default)]
    pub include_deprecated: bool,
    /// Management-only: include `retired` library nodes (default `false`) so the
    /// `/library` management view can show + reactivate them. The droppable
    /// palette never sets this. To list the complete management set, pass both
    /// `include_deprecated=true&include_retired=true`.
    #[serde(default)]
    pub include_retired: bool,
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
    superseded_by: Option<String>,
    presentation: Option<serde_json::Value>,
}

/// GET /api/v1/node-library
///
/// List the library nodes (branded, reusable `sub_workflow` building blocks)
/// the caller may drop onto a canvas. Returns the latest version of each
/// `library_node` family that is visible to the caller (public, or in the
/// caller's workspace). `deprecated` nodes are excluded unless
/// `include_deprecated=true`; `retired` nodes are excluded unless
/// `include_retired=true` (the `/library` management view passes both so it can
/// show + reactivate the complete set). Each row carries `myEffectiveRole` so
/// the management view can gate Manage/Demote to `admin`+. Ordered by category
/// → vendor → name so the palette can render its two-level grouping directly.
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
                version, name, description, origin, lifecycle_status, superseded_by, presentation \
           FROM workflow_templates \
          WHERE template_kind = 'library_node' \
            AND is_latest = TRUE \
            AND coordinate IS NOT NULL \
            AND ($3 OR lifecycle_status <> 'retired') \
            AND ($1 OR lifecycle_status = 'active') \
            AND (workspace_id = $2 OR visibility = 'public') \
          ORDER BY (presentation->>'category') NULLS LAST, \
                   (presentation->>'vendor') NULLS LAST, \
                   name",
    )
    .bind(params.include_deprecated)
    .bind(workspace_id)
    .bind(params.include_retired)
    .fetch_all(&state.db)
    .await?;

    let mut items: Vec<LibraryNodeDescriptor> = rows
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
            superseded_by: r.superseded_by,
            presentation: r
                .presentation
                .and_then(|v| serde_json::from_value::<Presentation>(v).ok()),
            my_effective_role: None,
        })
        .collect();

    // Stamp the caller's effective role per family root. `keep_all` (not
    // `filter_and_annotate_visible`): visibility is already enforced by the
    // WHERE clause (public-or-own-ws), so every visible row must render — the
    // role only gates the Manage/Demote action buttons.
    crate::auth::grants::annotate_roles_keep_all(
        &state.db,
        &user,
        crate::auth::ObjectKind::Template,
        workspace_id,
        &mut items,
    )
    .await
    .map_err(crate::auth::map_to_api_error)?;

    Ok(Json(items))
}

/// GET /api/v1/node-library/categories
///
/// The controlled category vocabulary a library node's `presentation.category`
/// must belong to (decision 6). Served from the single backend constant so the
/// promote form's category picker can never drift from what seed/promote
/// validation accepts.
#[utoipa::path(
    get,
    path = "/api/v1/node-library/categories",
    responses(
        (status = 200, description = "Controlled library category vocabulary", body = Vec<String>),
    ),
    tag = "node-library",
)]
pub async fn list_library_categories() -> Json<Vec<String>> {
    Json(
        crate::models::template::LIBRARY_CATEGORIES
            .iter()
            .map(|s| s.to_string())
            .collect(),
    )
}
