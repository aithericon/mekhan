use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
}

/// GET /healthz
///
/// Liveness probe — fixed shape, no DB / NATS dependencies. Lives at the
/// root (NOT under `/api/v1`) and OUTSIDE the auth layer so load balancers,
/// Nomad/k8s probes, and uptime monitors can poll it without a session
/// cookie. The path follows k8s convention; spec stays in the OpenAPI doc
/// for completeness but the runtime mount is intentionally separate from
/// the main protected `OpenApiRouter`.
#[utoipa::path(
    get,
    path = "/healthz",
    responses(
        (status = 200, description = "Service is alive", body = HealthResponse),
    ),
    tag = "health",
)]
pub async fn liveness() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_string(),
        service: "mekhan-service".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    })
}
