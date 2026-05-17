//! `/api/auth/tokens` — embedded per-user automation-token management.
//!
//! Every handler takes `user: AuthUser`, which re-runs the **cookie**
//! authenticator (`auth::extractor::FromRequestParts`) — never introspection.
//! So even though these routes sit behind `require_auth_middleware`, a Bearer
//! PAT can't reach them (no cookie ⇒ 401): a token cannot mint more tokens.
//! The caller's `subject` is the ownership boundary the broker enforces.
//!
//! Token validity lives entirely in Zitadel — these endpoints only broker the
//! Management API ([`crate::auth::ZitadelMgmt`]); Mekhan stores nothing.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};

use crate::auth::mgmt::{MgmtError, ZitadelMgmt};
use crate::auth::AuthUser;
use crate::models::auth_token::{CreatedToken, CreateTokenRequest, TokenSummary};
use crate::models::error::{ApiError, ErrorResponse};
use crate::AppState;

/// Resolve the broker, or 503 when token management isn't configured (no
/// `auth.broker_pat`) so the SPA can hide the section.
fn broker(state: &AppState) -> Result<&ZitadelMgmt, ApiError> {
    state.zitadel_mgmt.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "token management is not configured for this environment",
        )
    })
}

/// Map a broker failure to an HTTP error without leaking upstream detail.
fn map_err(e: MgmtError) -> ApiError {
    match e {
        MgmtError::NotFound => ApiError::not_found("token not found"),
        MgmtError::Upstream(detail) => {
            tracing::error!("zitadel management broker: {detail}");
            ApiError::new(StatusCode::BAD_GATEWAY, "token service upstream error")
        }
    }
}

/// GET /api/auth/tokens — the caller's automation tokens.
#[utoipa::path(
    get,
    path = "/api/auth/tokens",
    responses(
        (status = 200, description = "The caller's tokens", body = [TokenSummary]),
        (status = 401, description = "No session", body = ErrorResponse),
        (status = 503, description = "Token management disabled", body = ErrorResponse),
    ),
    tag = "auth-tokens",
)]
pub async fn list_tokens(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<TokenSummary>>, ApiError> {
    let tokens = broker(&state)?
        .list_tokens(&user.subject)
        .await
        .map_err(map_err)?;
    Ok(Json(tokens))
}

/// POST /api/auth/tokens — mint a token. The `secret` is returned once.
#[utoipa::path(
    post,
    path = "/api/auth/tokens",
    request_body = CreateTokenRequest,
    responses(
        (status = 200, description = "Token created (secret shown once)", body = CreatedToken),
        (status = 401, description = "No session", body = ErrorResponse),
        (status = 502, description = "Identity provider error", body = ErrorResponse),
        (status = 503, description = "Token management disabled", body = ErrorResponse),
    ),
    tag = "auth-tokens",
)]
pub async fn create_token(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateTokenRequest>,
) -> Result<Json<CreatedToken>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::bad_request("token name must not be empty"));
    }
    let created = broker(&state)?
        .create_token(
            &user.subject,
            req.name.trim(),
            req.description.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            req.expires_at.as_deref(),
        )
        .await
        .map_err(map_err)?;
    Ok(Json(created))
}

/// DELETE /api/auth/tokens/{id} — revoke a token (ownership-guarded).
#[utoipa::path(
    delete,
    path = "/api/auth/tokens/{id}",
    params(("id" = String, Path, description = "Token id from the list")),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "No session", body = ErrorResponse),
        (status = 404, description = "Unknown token, or not the caller's", body = ErrorResponse),
        (status = 503, description = "Token management disabled", body = ErrorResponse),
    ),
    tag = "auth-tokens",
)]
pub async fn revoke_token(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    broker(&state)?
        .revoke_token(&user.subject, &id)
        .await
        .map_err(map_err)?;
    Ok(StatusCode::NO_CONTENT)
}
