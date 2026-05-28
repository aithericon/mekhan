use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Uniform error body returned by every fallible handler. The spec exposes a
/// single `ErrorResponse` schema and the frontend gets one consistent error
/// shape: switch on `code` (machine-readable, stable kebab-case) for
/// programmatic handling; render `error` for humans.
///
/// `code` is populated automatically by every `ApiError::*` constructor.
/// Stable values: `"bad-request"`, `"unauthorized"`, `"forbidden"`,
/// `"not-found"`, `"conflict"`, `"precondition-failed"`, `"internal"`,
/// `"compile-failed"`, `"publish-gate"`. Absent only on the few
/// extractor-level rejections that don't flow through `ApiError`.
///
/// Optional `compile_errors` carries structured per-edge / per-node failures
/// from the workflow compiler so the editor can highlight inline.
/// Optional `failing_tests` carries the publish gate's per-test detail.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compile_errors: Option<Vec<crate::compiler::CompileErrorView>>,
    /// Structured per-test failures returned by the publication gate when a
    /// publish is blocked. Absent on non-gate errors. The shape mirrors
    /// `models::template_test::FailingTestInfo`; declared as `Value` here to
    /// avoid pulling the dependency into this module's signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing_tests: Option<serde_json::Value>,
}

impl ErrorResponse {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            code: None,
            compile_errors: None,
            failing_tests: None,
        }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    pub fn with_compile_errors(mut self, views: Vec<crate::compiler::CompileErrorView>) -> Self {
        self.compile_errors = Some(views);
        self
    }

    /// Attach the publish gate's failing-test list so the editor can render
    /// per-test detail in `PublishGateModal`. The value is serialized as JSON
    /// to keep this module dependency-free.
    pub fn with_failing_tests(mut self, value: serde_json::Value) -> Self {
        self.failing_tests = Some(value);
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
    /// Stable kebab-case code inferred from the status. Handlers that need a
    /// more specific code (e.g. `"compile-failed"`, `"publish-gate"`) build
    /// `ErrorResponse` directly and call `.with_code()` before constructing
    /// the `ApiError`.
    fn default_code(status: StatusCode) -> &'static str {
        match status {
            StatusCode::BAD_REQUEST => "bad-request",
            StatusCode::UNAUTHORIZED => "unauthorized",
            StatusCode::FORBIDDEN => "forbidden",
            StatusCode::NOT_FOUND => "not-found",
            StatusCode::CONFLICT => "conflict",
            StatusCode::PRECONDITION_FAILED => "precondition-failed",
            StatusCode::UNPROCESSABLE_ENTITY => "unprocessable",
            StatusCode::TOO_MANY_REQUESTS => "rate-limited",
            StatusCode::SERVICE_UNAVAILABLE => "unavailable",
            _ if status.is_server_error() => "internal",
            _ => "error",
        }
    }

    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        let code = Self::default_code(status);
        Self {
            status,
            body: Some(ErrorResponse::new(message).with_code(code)),
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

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
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
            body: Some(
                ErrorResponse::new(message)
                    .with_code("compile-failed")
                    .with_compile_errors(views),
            ),
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
