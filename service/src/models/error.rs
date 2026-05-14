use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Uniform error body returned by every fallible handler. Replaces the ad-hoc
/// `json!({"error": ...})` pattern so the spec exposes a single
/// `ErrorResponse` schema and the frontend gets one consistent error shape.
///
/// Optional `compile_errors` carries structured per-edge / per-node failures
/// from the workflow compiler so the editor can highlight inline (Phase 2
/// typed-ports). Absent on non-compile errors.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compile_errors: Option<Vec<crate::compiler::CompileErrorView>>,
}

impl ErrorResponse {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            compile_errors: None,
        }
    }

    pub fn with_compile_errors(mut self, views: Vec<crate::compiler::CompileErrorView>) -> Self {
        self.compile_errors = Some(views);
        self
    }
}

/// Bundles a status code with an optional `ErrorResponse` body so handlers can
/// return `Result<Json<T>, ApiError>` and let `?` propagate. When `body` is
/// `None` the response is the bare status code with an empty body — used to
/// preserve the existing wire format for handlers that historically returned
/// e.g. `StatusCode::NOT_FOUND.into_response()` directly.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub body: Option<ErrorResponse>,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: Some(ErrorResponse::new(message)),
        }
    }

    /// Returns a bare status code with no body — preserves wire format for the
    /// pre-existing `StatusCode::X.into_response()` pattern.
    pub fn status_only(status: StatusCode) -> Self {
        Self { status, body: None }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    /// Bad-request error that attaches a structured `compile_errors` payload
    /// so the editor can highlight inline. The handler stays in control of
    /// the surrounding human-readable message (kept terse to avoid duplicating
    /// every per-edge message in the top-line `error`).
    pub fn compile(
        message: impl Into<String>,
        views: Vec<crate::compiler::CompileErrorView>,
    ) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: Some(ErrorResponse::new(message).with_compile_errors(views)),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self.body {
            Some(body) => (self.status, Json(body)).into_response(),
            None => self.status.into_response(),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Self::not_found("not found"),
            other => Self::internal(other.to_string()),
        }
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::internal(e.to_string())
    }
}
