//! Backend registry HTTP handler — exposes the per-backend metadata the
//! frontend needs to drive its picker, default config seed, and "Reset to
//! default" output port. Backed by `crate::backends::BACKENDS`.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use crate::backends::{descriptors, lookup, BackendDescriptor, OutputAuthoring};
use crate::models::template::{ExecutionBackendType, Port};

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
    // Only authorable backends reach the editor's picker. Non-authorable
    // variants (e.g. `llm`, authored via the Agent node instead) stay
    // fully compilable/runnable — they're just not offered as a standalone
    // AutomatedStep backend.
    Json(
        descriptors()
            .into_iter()
            .filter(|d| d.user_authorable)
            .collect(),
    )
}

/// POST /api/v1/backends/{name}/derive-output
///
/// Compute the canonical output [`Port`] for an AutomatedStep with this
/// backend, given its current config. Frontend calls this for
/// `output_authoring == "derived"` backends (LLM today) whenever the
/// step's config changes, so the read-only port editor always reflects
/// the actual runtime envelope.
///
/// Permissive: a half-typed config (missing fields, partial schema)
/// returns the closest valid port — not an error. Hard validation runs at
/// publish time via `BackendDecl::validate`.
///
/// Returns:
/// - 200 with the derived [`Port`] on success.
/// - 400 when the backend exists but isn't `Derived` (callers should use
///   `default_output_port` from `GET /api/v1/backends` for `Fixed` /
///   `Free` backends).
/// - 404 when the backend name doesn't resolve.
#[utoipa::path(
    post,
    path = "/api/v1/backends/{name}/derive-output",
    params(
        ("name" = String, Path, description = "Backend wire name (e.g. `llm`, `python`)"),
    ),
    request_body = serde_json::Value,
    responses(
        (status = 200, description = "Derived output port", body = Port),
        (status = 400, description = "Backend's output is not derived from config"),
        (status = 404, description = "Unknown backend"),
    ),
    tag = "backends",
)]
pub async fn derive_backend_output(
    Path(name): Path<String>,
    Json(config): Json<Value>,
) -> Response {
    let Some(backend_type) = ExecutionBackendType::from_wire_str(&name) else {
        return (
            StatusCode::NOT_FOUND,
            format!("unknown backend: {name}"),
        )
            .into_response();
    };
    let Some(decl) = lookup(backend_type) else {
        return (
            StatusCode::NOT_FOUND,
            format!("backend not registered: {name}"),
        )
            .into_response();
    };
    match (decl.output_authoring, decl.derive_output_port) {
        (OutputAuthoring::Derived, Some(derive)) => Json(derive(&config)).into_response(),
        (OutputAuthoring::Derived, None) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "backend `{name}` claims output_authoring=Derived but has no derive_output_port"
            ),
        )
            .into_response(),
        _ => (
            StatusCode::BAD_REQUEST,
            format!(
                "backend `{name}` does not derive its output from config (authoring={:?}); use the default_output_port from GET /api/v1/backends instead",
                decl.output_authoring
            ),
        )
            .into_response(),
    }
}
