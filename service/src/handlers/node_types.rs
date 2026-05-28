//! Node-type registry HTTP handler — exposes the per-variant metadata the
//! frontend uses to drive its palette, property-panel routing, and
//! port-derivation helpers. Backed by `crate::nodes::NODES`.

use axum::Json;

use crate::nodes::{descriptors, NodeDescriptor};

/// GET /api/v1/node-types
///
/// List every registered workflow node type with its display metadata,
/// runtime kind, and protocol flags. The frontend reads this once per
/// session (cached alongside the backends registry) and drives the
/// editor's palette + property-panel dispatch from it. The Svelte
/// component map (`lib/components/editor/nodes/index.ts`) and the Lucide
/// icon map stay hand-written — components and icon imports can't be
/// serialized through JSON — but the palette label, description, kind,
/// and protocol flags flow from here.
#[utoipa::path(
    get,
    path = "/api/v1/node-types",
    responses(
        (status = 200, description = "Registered node types", body = Vec<NodeDescriptor>),
    ),
    tag = "node-types",
)]
pub async fn list_node_types() -> Json<Vec<NodeDescriptor>> {
    Json(descriptors())
}
