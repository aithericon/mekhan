//! Backend registry HTTP handler — exposes the per-backend metadata the
//! frontend needs to drive its picker, default config seed, and "Reset to
//! default" output port. Backed by `crate::backends::BACKENDS`.

use axum::Json;

use crate::backends::{descriptors, BackendDescriptor};

/// GET /api/v1/backends
///
/// List every registered backend with its display metadata, default editor
/// config, default output port shape, dispatch mode (executor job vs.
/// engine effect), resource channel (staged file vs. config overlay), and
/// schedulability.
///
/// The frontend reads this once per session (cached in
/// `app/src/lib/editor/backend-registry.svelte.ts`) and drives the
/// AutomatedStep editor panel from it — backend picker label/icon,
/// default config seed, default output port. The Svelte config-panel
/// component map (`backend-panels.ts`) stays hand-written; everything
/// else flows from here.
#[utoipa::path(
    get,
    path = "/api/v1/backends",
    responses(
        (status = 200, description = "Registered backends", body = Vec<BackendDescriptor>),
    ),
    tag = "backends",
)]
pub async fn list_backends() -> Json<Vec<BackendDescriptor>> {
    Json(descriptors())
}
