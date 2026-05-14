use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    pub status: String,
    pub service: String,
    pub version: String,
}

/// GET /health
///
/// Liveness probe — fixed shape, no DB / NATS dependencies.
#[utoipa::path(
    get,
    path = "/health",
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
