//! `/api/v1/auth/tokens` — embedded per-user automation-token management,
//! mekhan-native.
//!
//! Each "token" is one row in the local `user_pats` table: mekhan owns the
//! credential outright (a `uat_{id}.{secret}` PAT), storing only the SHA-256 of
//! the secret half — no Zitadel broker, no IdP round-trip, works offline in
//! `dev_noop`. The presented `uat_` bearer authenticates against this same table
//! via [`crate::auth::user_pat`].
//!
//! Every handler takes [`CookieAuthUser`] — the explicit **cookie-only**
//! extractor. Other endpoints use the dual-use `AuthUser` (Bearer or cookie),
//! but here, even behind `require_auth_middleware`, a Bearer PAT is refused (no
//! session cookie ⇒ 401): a token can never be used to mint or revoke tokens.
//! The caller's resolved `user_id` is the ownership boundary.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};

use crate::auth::extractor::CookieAuthUser;
use crate::models::auth_token::{CreateTokenRequest, CreatedToken, TokenSummary};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::runner::{mint_token, USER_PAT_TOKEN_PREFIX};
use crate::AppState;

/// One `user_pats` row, projected for the list endpoint.
#[derive(sqlx::FromRow)]
struct TokenRow {
    id: uuid::Uuid,
    name: String,
    description: Option<String>,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
}

/// GET /api/v1/auth/tokens — the caller's automation tokens.
#[utoipa::path(
    get,
    path = "/api/v1/auth/tokens",
    responses(
        (status = 200, description = "The caller's tokens", body = [TokenSummary]),
        (status = 401, description = "No session", body = ErrorResponse),
    ),
    tag = "auth-tokens",
)]
pub async fn list_tokens(
    State(state): State<AppState>,
    CookieAuthUser(user): CookieAuthUser,
) -> Result<Json<Vec<TokenSummary>>, ApiError> {
    let rows = sqlx::query_as::<_, TokenRow>(
        "SELECT id, name, description, created_at, expires_at \
           FROM user_pats \
          WHERE user_id = $1 AND revoked_at IS NULL \
          ORDER BY created_at",
    )
    .bind(user.subject_as_uuid())
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("list tokens: {e}")))?;

    let tokens = rows
        .into_iter()
        .map(|r| TokenSummary {
            id: r.id.to_string(),
            name: r.name,
            description: r.description,
            created_at: Some(r.created_at.to_rfc3339()),
            expires_at: r.expires_at.map(|t| t.to_rfc3339()),
        })
        .collect();
    Ok(Json(tokens))
}

/// POST /api/v1/auth/tokens — mint a token. The `secret` is returned once.
#[utoipa::path(
    post,
    path = "/api/v1/auth/tokens",
    request_body = CreateTokenRequest,
    responses(
        (status = 200, description = "Token created (secret shown once)", body = CreatedToken),
        (status = 400, description = "Invalid request", body = ErrorResponse),
        (status = 401, description = "No session", body = ErrorResponse),
    ),
    tag = "auth-tokens",
)]
pub async fn create_token(
    State(state): State<AppState>,
    CookieAuthUser(user): CookieAuthUser,
    Json(req): Json<CreateTokenRequest>,
) -> Result<Json<CreatedToken>, ApiError> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("token name must not be empty"));
    }
    let description = req
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Parse the optional RFC 3339 expiry up-front so a bad value 400s before we
    // touch the DB. The wire value is echoed back verbatim in the response.
    let expires_at: Option<DateTime<Utc>> = match req.expires_at.as_deref() {
        Some(raw) => Some(
            DateTime::parse_from_rfc3339(raw)
                .map_err(|_| ApiError::bad_request("expires_at must be an RFC 3339 timestamp"))?
                .with_timezone(&Utc),
        ),
        None => None,
    };

    let id = uuid::Uuid::new_v4();
    let minted = mint_token(USER_PAT_TOKEN_PREFIX, id);
    let now = Utc::now();

    sqlx::query(
        "INSERT INTO user_pats (id, user_id, name, description, token_hash, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(user.subject_as_uuid())
    .bind(name)
    .bind(&description)
    .bind(&minted.token_hash)
    .bind(now)
    .bind(expires_at)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("create token: {e}")))?;

    Ok(Json(CreatedToken {
        id: id.to_string(),
        name: name.to_string(),
        description,
        created_at: Some(now.to_rfc3339()),
        expires_at: req.expires_at,
        secret: minted.full_token,
    }))
}

/// DELETE /api/v1/auth/tokens/{id} — revoke a token (ownership-guarded).
#[utoipa::path(
    delete,
    path = "/api/v1/auth/tokens/{id}",
    params(("id" = String, Path, description = "Token id from the list")),
    responses(
        (status = 204, description = "Revoked"),
        (status = 401, description = "No session", body = ErrorResponse),
        (status = 404, description = "Unknown token, or not the caller's", body = ErrorResponse),
    ),
    tag = "auth-tokens",
)]
pub async fn revoke_token(
    State(state): State<AppState>,
    CookieAuthUser(user): CookieAuthUser,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    // A malformed id is indistinguishable from "not yours" → 404, never 400, so
    // we don't leak whether a token id exists.
    let token_id =
        uuid::Uuid::parse_str(&id).map_err(|_| ApiError::not_found("token not found"))?;

    let result = sqlx::query(
        "UPDATE user_pats SET revoked_at = now() \
          WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
    )
    .bind(token_id)
    .bind(user.subject_as_uuid())
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("revoke token: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("token not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
