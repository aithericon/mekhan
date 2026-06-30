//! Workspace service accounts — CRUD + token lifecycle (mirrors `invites.rs`).
//!
//! A service account is a NON-human API principal OWNED BY A WORKSPACE: it
//! carries a fixed workspace role and survives member offboarding (it dies only
//! when disabled or its token is revoked). These MANAGEMENT endpoints mint and
//! govern service accounts, so they are doubly gated:
//!
//!   1. The caller MUST be a HUMAN principal — every endpoint first rejects any
//!      machine principal (`runner:`/`worker:`/`service-account:` subject) via
//!      [`crate::auth::is_machine_principal`], so a service account can never
//!      mint MORE service accounts (lateral movement / privilege
//!      self-replication). This is the same intentional privilege-escalation
//!      guard `auth_tokens.rs` makes with `CookieAuthUser`, but stated at the
//!      call site so legitimate human `uat_` CI PATs still work.
//!   2. The caller MUST be a workspace Admin/Owner via
//!      [`crate::auth::require_workspace_admin`].
//!
//! Every nested route scopes by `workspace_id` in its WHERE clause (or a JOIN to
//! `service_accounts`) so a `sa_id`/`token_id` from another workspace can never
//! be reached.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use uuid::Uuid;

use crate::auth::{is_machine_principal, map_to_api_error, require_workspace_admin, AuthUser};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::runner::{mint_token, SERVICE_ACCOUNT_TOKEN_PREFIX};
use crate::models::service_account::{
    CreateServiceAccountRequest, CreateServiceAccountTokenRequest, CreatedServiceAccountToken,
    PatchServiceAccountRequest, ServiceAccountRow, ServiceAccountSummary, ServiceAccountTokenRow,
    ServiceAccountTokenSummary,
};
use crate::AppState;

/// The roles a service account may hold — `owner` is deliberately excluded (a SA
/// may never be a workspace owner). Enforced here AND by the DB CHECK.
const ALLOWED_SA_ROLES: [&str; 3] = ["viewer", "editor", "admin"];

/// Shared FIRST gate for every SA-management endpoint: reject machine principals,
/// then require the human caller to be a workspace Admin/Owner.
async fn gate_human_admin(
    state: &AppState,
    user: &AuthUser,
    workspace_id: Uuid,
) -> Result<(), ApiError> {
    if is_machine_principal(user) {
        return Err(ApiError::forbidden(
            "service accounts cannot manage service accounts",
        ));
    }
    require_workspace_admin(&state.db, user, workspace_id)
        .await
        .map_err(map_to_api_error)?;
    Ok(())
}

/// Confirm a service account exists AND belongs to `workspace_id`. Returns a 404
/// (never a cross-workspace leak) when the id is unknown or lives elsewhere.
async fn assert_sa_in_workspace(
    state: &AppState,
    workspace_id: Uuid,
    sa_id: Uuid,
) -> Result<(), ApiError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM service_accounts WHERE id = $1 AND workspace_id = $2")
            .bind(sa_id)
            .bind(workspace_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| ApiError::internal(format!("service account lookup: {e}")))?;
    if row.is_none() {
        return Err(ApiError::not_found("service account not found"));
    }
    Ok(())
}

// ── service-account CRUD ───────────────────────────────────────────────────

/// POST /api/v1/workspaces/{workspace_id}/service-accounts — human-Admin-gated.
#[utoipa::path(
    post,
    path = "/api/v1/workspaces/{workspace_id}/service-accounts",
    params(("workspace_id" = Uuid, Path, description = "Workspace id")),
    request_body = CreateServiceAccountRequest,
    responses(
        (status = 200, description = "Service account created", body = ServiceAccountSummary),
        (status = 400, description = "Invalid name/role", body = ErrorResponse),
        (status = 403, description = "Admin role required / machine principal", body = ErrorResponse),
        (status = 409, description = "Name already in use", body = ErrorResponse),
    ),
    tag = "service-accounts",
)]
pub async fn create_service_account(
    State(state): State<AppState>,
    user: AuthUser,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<CreateServiceAccountRequest>,
) -> Result<Json<ServiceAccountSummary>, ApiError> {
    gate_human_admin(&state, &user, workspace_id).await?;

    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "service account name must not be empty",
        ));
    }
    let role = req.role.trim();
    if !ALLOWED_SA_ROLES.contains(&role) {
        return Err(ApiError::bad_request(
            "role must be one of viewer|editor|admin (a service account may not be owner)",
        ));
    }
    let description = req
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let row = sqlx::query_as::<_, ServiceAccountRow>(
        "INSERT INTO service_accounts (workspace_id, name, description, role, created_by) \
              VALUES ($1, $2, $3, $4, $5) \
         RETURNING id, workspace_id, name, description, role, created_by, created_at, disabled_at",
    )
    .bind(workspace_id)
    .bind(name)
    .bind(&description)
    .bind(role)
    .bind(user.subject_as_uuid())
    .fetch_one(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::conflict("a service account with that name already exists")
        }
        _ => ApiError::internal(format!("create service account: {e}")),
    })?;

    Ok(Json(row.into()))
}

/// GET /api/v1/workspaces/{workspace_id}/service-accounts — human-Admin-gated
/// list of ALL service accounts in the workspace (they are workspace-owned).
#[utoipa::path(
    get,
    path = "/api/v1/workspaces/{workspace_id}/service-accounts",
    params(("workspace_id" = Uuid, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Service accounts for the workspace", body = [ServiceAccountSummary]),
        (status = 403, description = "Admin role required / machine principal", body = ErrorResponse),
    ),
    tag = "service-accounts",
)]
pub async fn list_service_accounts(
    State(state): State<AppState>,
    user: AuthUser,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Vec<ServiceAccountSummary>>, ApiError> {
    gate_human_admin(&state, &user, workspace_id).await?;

    let rows = sqlx::query_as::<_, ServiceAccountRow>(
        "SELECT id, workspace_id, name, description, role, created_by, created_at, disabled_at \
           FROM service_accounts WHERE workspace_id = $1 ORDER BY created_at",
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("list service accounts: {e}")))?;

    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// PATCH .../service-accounts/{sa_id} — rename and/or toggle the disabled state.
#[utoipa::path(
    patch,
    path = "/api/v1/workspaces/{workspace_id}/service-accounts/{sa_id}",
    params(
        ("workspace_id" = Uuid, Path, description = "Workspace id"),
        ("sa_id" = Uuid, Path, description = "Service account id"),
    ),
    request_body = PatchServiceAccountRequest,
    responses(
        (status = 200, description = "Updated service account", body = ServiceAccountSummary),
        (status = 400, description = "Invalid name", body = ErrorResponse),
        (status = 403, description = "Admin role required / machine principal", body = ErrorResponse),
        (status = 404, description = "Service account not found", body = ErrorResponse),
        (status = 409, description = "Name already in use", body = ErrorResponse),
    ),
    tag = "service-accounts",
)]
pub async fn patch_service_account(
    State(state): State<AppState>,
    user: AuthUser,
    Path((workspace_id, sa_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<PatchServiceAccountRequest>,
) -> Result<Json<ServiceAccountSummary>, ApiError> {
    gate_human_admin(&state, &user, workspace_id).await?;

    // Normalise the optional rename up front so an all-whitespace name 400s.
    let new_name = match req.name.as_deref().map(str::trim) {
        Some("") => {
            return Err(ApiError::bad_request(
                "service account name must not be empty",
            ))
        }
        Some(n) => Some(n.to_string()),
        None => None,
    };

    // COALESCE the rename; toggle disabled_at only when `disabled` is supplied
    // (true ⇒ now(), false ⇒ clear). The `AND workspace_id` guard prevents
    // cross-workspace id confusion.
    let row = sqlx::query_as::<_, ServiceAccountRow>(
        "UPDATE service_accounts SET \
                name = COALESCE($3, name), \
                disabled_at = CASE \
                    WHEN $4::bool IS NULL THEN disabled_at \
                    WHEN $4::bool THEN now() \
                    ELSE NULL END \
          WHERE id = $1 AND workspace_id = $2 \
         RETURNING id, workspace_id, name, description, role, created_by, created_at, disabled_at",
    )
    .bind(sa_id)
    .bind(workspace_id)
    .bind(new_name)
    .bind(req.disabled)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::conflict("a service account with that name already exists")
        }
        _ => ApiError::internal(format!("update service account: {e}")),
    })?
    .ok_or_else(|| ApiError::not_found("service account not found"))?;

    Ok(Json(row.into()))
}

/// DELETE .../service-accounts/{sa_id} — hard delete (CASCADE drops its tokens).
#[utoipa::path(
    delete,
    path = "/api/v1/workspaces/{workspace_id}/service-accounts/{sa_id}",
    params(
        ("workspace_id" = Uuid, Path, description = "Workspace id"),
        ("sa_id" = Uuid, Path, description = "Service account id"),
    ),
    responses(
        (status = 204, description = "Deleted"),
        (status = 403, description = "Admin role required / machine principal", body = ErrorResponse),
        (status = 404, description = "Service account not found", body = ErrorResponse),
    ),
    tag = "service-accounts",
)]
pub async fn delete_service_account(
    State(state): State<AppState>,
    user: AuthUser,
    Path((workspace_id, sa_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    gate_human_admin(&state, &user, workspace_id).await?;

    let result = sqlx::query("DELETE FROM service_accounts WHERE id = $1 AND workspace_id = $2")
        .bind(sa_id)
        .bind(workspace_id)
        .execute(&state.db)
        .await
        .map_err(|e| ApiError::internal(format!("delete service account: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("service account not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── service-account tokens ─────────────────────────────────────────────────

/// POST .../service-accounts/{sa_id}/tokens — mint a `sat_` token; the secret is
/// returned ONCE.
#[utoipa::path(
    post,
    path = "/api/v1/workspaces/{workspace_id}/service-accounts/{sa_id}/tokens",
    params(
        ("workspace_id" = Uuid, Path, description = "Workspace id"),
        ("sa_id" = Uuid, Path, description = "Service account id"),
    ),
    request_body = CreateServiceAccountTokenRequest,
    responses(
        (status = 200, description = "Token created (secret shown once)", body = CreatedServiceAccountToken),
        (status = 400, description = "Invalid name", body = ErrorResponse),
        (status = 403, description = "Admin role required / machine principal", body = ErrorResponse),
        (status = 404, description = "Service account not found", body = ErrorResponse),
    ),
    tag = "service-accounts",
)]
pub async fn create_sa_token(
    State(state): State<AppState>,
    user: AuthUser,
    Path((workspace_id, sa_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<CreateServiceAccountTokenRequest>,
) -> Result<Json<CreatedServiceAccountToken>, ApiError> {
    gate_human_admin(&state, &user, workspace_id).await?;
    assert_sa_in_workspace(&state, workspace_id, sa_id).await?;

    let name = req.name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("token name must not be empty"));
    }

    let id = Uuid::new_v4();
    let minted = mint_token(SERVICE_ACCOUNT_TOKEN_PREFIX, id);

    sqlx::query(
        "INSERT INTO service_account_tokens (id, service_account_id, name, token_hash, expires_at) \
              VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(sa_id)
    .bind(name)
    .bind(&minted.token_hash)
    .bind(req.expires_at)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("create service account token: {e}")))?;

    Ok(Json(CreatedServiceAccountToken {
        id,
        name: name.to_string(),
        expires_at: req.expires_at,
        secret: minted.full_token,
    }))
}

/// GET .../service-accounts/{sa_id}/tokens — metadata only; NEVER the secret.
#[utoipa::path(
    get,
    path = "/api/v1/workspaces/{workspace_id}/service-accounts/{sa_id}/tokens",
    params(
        ("workspace_id" = Uuid, Path, description = "Workspace id"),
        ("sa_id" = Uuid, Path, description = "Service account id"),
    ),
    responses(
        (status = 200, description = "Tokens for the service account", body = [ServiceAccountTokenSummary]),
        (status = 403, description = "Admin role required / machine principal", body = ErrorResponse),
        (status = 404, description = "Service account not found", body = ErrorResponse),
    ),
    tag = "service-accounts",
)]
pub async fn list_sa_tokens(
    State(state): State<AppState>,
    user: AuthUser,
    Path((workspace_id, sa_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<ServiceAccountTokenSummary>>, ApiError> {
    gate_human_admin(&state, &user, workspace_id).await?;
    assert_sa_in_workspace(&state, workspace_id, sa_id).await?;

    let rows = sqlx::query_as::<_, ServiceAccountTokenRow>(
        "SELECT id, service_account_id, name, token_hash, created_at, expires_at, \
                last_used_at, revoked_at \
           FROM service_account_tokens \
          WHERE service_account_id = $1 AND revoked_at IS NULL \
          ORDER BY created_at",
    )
    .bind(sa_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("list service account tokens: {e}")))?;

    Ok(Json(rows.into_iter().map(Into::into).collect()))
}

/// DELETE .../service-accounts/{sa_id}/tokens/{token_id} — revoke a token. The
/// JOIN to `service_accounts` re-asserts the token's SA is in `workspace_id`.
#[utoipa::path(
    delete,
    path = "/api/v1/workspaces/{workspace_id}/service-accounts/{sa_id}/tokens/{token_id}",
    params(
        ("workspace_id" = Uuid, Path, description = "Workspace id"),
        ("sa_id" = Uuid, Path, description = "Service account id"),
        ("token_id" = Uuid, Path, description = "Token id"),
    ),
    responses(
        (status = 204, description = "Revoked"),
        (status = 403, description = "Admin role required / machine principal", body = ErrorResponse),
        (status = 404, description = "Token not found", body = ErrorResponse),
    ),
    tag = "service-accounts",
)]
pub async fn revoke_sa_token(
    State(state): State<AppState>,
    user: AuthUser,
    Path((workspace_id, sa_id, token_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    gate_human_admin(&state, &user, workspace_id).await?;

    // Single statement scoped by token id + SA id + workspace id (the JOIN): an
    // unknown token, a token of another SA, or another workspace's token all
    // affect zero rows ⇒ uniform 404.
    let result = sqlx::query(
        "UPDATE service_account_tokens t SET revoked_at = now() \
           FROM service_accounts sa \
          WHERE t.id = $1 AND t.service_account_id = $2 \
            AND sa.id = t.service_account_id AND sa.workspace_id = $3 \
            AND t.revoked_at IS NULL",
    )
    .bind(token_id)
    .bind(sa_id)
    .bind(workspace_id)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("revoke service account token: {e}")))?;

    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("token not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}
