//! Phase 1 — Lab Runner Fleet endpoints.
//!
//! Three auth tiers under the `runners` tag:
//!
//!   - `POST /api/v1/runners/enroll` — PUBLIC (no auth gate). Authenticated by
//!     the `rt_` registration token in the body. Mints a runner, returns its
//!     `rnr_` credential ONCE.
//!   - `POST /api/v1/runners/{id}/heartbeat` — runner-token authed (the
//!     `AuthUser` resolved from the `rnr_` bearer). A runner may only heartbeat
//!     itself.
//!   - Management (human): list/get/revoke runners + registration-token
//!     mint/list/revoke. Reads use the dual-use `AuthUser`; the mint route uses
//!     [`CookieAuthUser`] — the same browser-only boundary as `auth_tokens.rs`,
//!     so a machine token can never mint enrollment secrets.
//!
//! Only the SHA-256 of each secret is stored; plaintext is surfaced once.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::extractor::CookieAuthUser;
use crate::auth::runner_token::runner_subject;
use crate::auth::AuthUser;
use crate::models::capability::{load_known_capabilities, validate_caps_against_types};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::runner::{
    mint_token, CreateRegistrationTokenRequest, CreatedRegistrationToken, EnrollRequest,
    EnrolledRunner, RegistrationTokenSummary, RunnerDetail, RunnerInterfaceCatalog,
    RunnerInterfaces, RunnerNatsCreds, RunnerPresenceSnapshot, RunnerRegistrationTokenRow,
    RunnerRow, RunnerSummary, UpsertRunnerInterfacesRequest, REG_TOKEN_PREFIX, RUNNER_TOKEN_PREFIX,
};
use crate::models::template::PaginatedResponse;
use crate::AppState;

/// Caller-implicit workspace: the principal's session workspace, falling back
/// to `Uuid::nil()` for the legacy no-workspace dev shape. Mirrors
/// `resources::caller_workspace`.
fn caller_workspace(user: &AuthUser) -> Uuid {
    user.workspace_id.unwrap_or_else(Uuid::nil)
}

/// A `group` alias is interpolated verbatim into a NATS subject (`{group}.claim`)
/// in the minted runner JWT, so it must be a single safe subject token — no
/// `.` (extra tokens), `*`/`>` (wildcards), or whitespace that would broaden
/// the granted publish permission. Empty is rejected here; callers treat
/// `None`/empty as "no group" before this point.
fn is_safe_group(group: &str) -> bool {
    !group.is_empty()
        && group
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Whether a live presence-backed `capacity` resource named `alias` exists in
/// `workspace_id`.
///
/// A runner's `group` is only meaningful if it is BACKED by a `capacity` resource
/// sitting at the INSTRUMENT point in the trait-space: `presence` liveness (the
/// `instrument` preset — see `models/capacity.rs::presets`). That resource is what
/// carries the presence-pool net (`pool-<resource_id>`, deployed at
/// resource-create) the presence controller admits the runner's unit into.
/// Without it, a heartbeating runner is tracked "liveness-only" and is never
/// admitted to any capacity pool — a silent dangling reference. We forbid minting
/// a registration token for an unbacked group so the dangling reference can't be
/// created at its source; the operator must create the group (a `capacity`
/// resource, `instrument` preset) first.
///
/// This is the runner analogue of `workers::worker_group_exists`, which gates on
/// the WORKER (`competing_consumer` + `auto`) point: a runner group is the presence- /
/// presence-admission path, so it gates on `liveness = 'presence'` instead. The
/// axis lives in the latest version's `public_config`, so this joins `resources`
/// → `resource_versions` at `latest_version` and matches the presence liveness —
/// the same lookup `runners_presence::resolve_pool_net_id` uses, so the gate and
/// the runtime admission agree on what "the group exists" means.
async fn runner_group_exists(
    db: &sqlx::PgPool,
    workspace_id: Uuid,
    alias: &str,
) -> Result<bool, ApiError> {
    let found: Option<(Uuid,)> = sqlx::query_as::<_, (Uuid,)>(
        "SELECT r.id FROM resources r \
         JOIN resource_versions rv \
           ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.workspace_id = $1 AND r.path = $2 \
           AND r.resource_type = 'capacity' AND r.deleted_at IS NULL \
           AND rv.public_config ->> 'liveness' = 'presence'",
    )
    .bind(workspace_id)
    .bind(alias)
    .fetch_optional(db)
    .await?;
    Ok(found.is_some())
}

// ── Query params ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ListRunnersQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ListRegTokensQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
}

fn default_page() -> i64 {
    1
}
fn default_per_page() -> i64 {
    20
}

// ── a. Enroll (public) ─────────────────────────────────────────────────────

/// `POST /api/v1/runners/enroll` — GitLab-style enrollment. PUBLIC: authed by
/// the `rt_` token in the body. The enrolled runner inherits the registration
/// token's `workspace_id` + `group`; `enrolled_by` is the token's `created_by`.
#[utoipa::path(
    post,
    path = "/api/v1/runners/enroll",
    request_body = EnrollRequest,
    responses(
        (status = 201, description = "Runner enrolled (runner_token shown once)", body = EnrolledRunner),
        (status = 401, description = "Invalid registration token", body = ErrorResponse),
        (status = 403, description = "Revoked / expired / exhausted registration token", body = ErrorResponse),
        (status = 409, description = "Runner name already exists in workspace", body = ErrorResponse),
    ),
    tag = "runners",
)]
pub async fn enroll_runner(
    State(state): State<AppState>,
    Json(req): Json<EnrollRequest>,
) -> Result<(StatusCode, Json<EnrolledRunner>), ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::bad_request("runner name must not be empty"));
    }

    // 1. Parse + look up the registration token row by its uuid prefix.
    let (reg_id, secret) =
        crate::models::runner::parse_token(REG_TOKEN_PREFIX, &req.registration_token).ok_or_else(
            || ApiError::new(StatusCode::UNAUTHORIZED, "malformed registration token"),
        )?;

    let reg = sqlx::query_as::<_, RunnerRegistrationTokenRow>(
        "SELECT * FROM runner_registration_tokens WHERE id = $1",
    )
    .bind(reg_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "unknown registration token"))?;

    // 2. Constant-time secret verification.
    if !crate::models::runner::verify_secret(&secret, &reg.token_hash) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "registration token mismatch",
        ));
    }

    // 3. Validity gates → 403.
    if reg.revoked_at.is_some() {
        return Err(ApiError::forbidden("registration token has been revoked"));
    }
    if let Some(expires_at) = reg.expires_at {
        if expires_at <= Utc::now() {
            return Err(ApiError::forbidden("registration token has expired"));
        }
    }
    if let Some(max) = reg.max_uses {
        if reg.uses >= max {
            return Err(ApiError::forbidden(
                "registration token has reached its max_uses",
            ));
        }
    }
    // Non-reusable token is single-use: exhausted once it has enrolled once.
    if !reg.reusable && reg.uses >= 1 {
        return Err(ApiError::forbidden(
            "single-use registration token already consumed",
        ));
    }

    // 4. Atomically claim a use of the registration token. This guarded UPDATE
    //    — not the friendly checks above — is the real authorization gate: the
    //    checks at step 3 are read-then-act and thus racy, so the WHERE here
    //    re-validates every condition while incrementing. Two concurrent
    //    enrolls of a single-use / near-`max_uses` token therefore cannot both
    //    succeed: at most one UPDATE matches and bumps `uses`. The secret was
    //    already verified constant-time against the fetched row above; the
    //    whole claim+insert runs in one transaction so a runner-name collision
    //    rolls the claimed use back instead of burning it.
    let mut tx = state.db.begin().await?;

    let claimed = sqlx::query_as::<_, RunnerRegistrationTokenRow>(
        "UPDATE runner_registration_tokens \
            SET uses = uses + 1 \
          WHERE id = $1 \
            AND revoked_at IS NULL \
            AND (expires_at IS NULL OR expires_at > NOW()) \
            AND (max_uses IS NULL OR uses < max_uses) \
            AND (reusable OR uses < 1) \
          RETURNING *",
    )
    .bind(reg.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        ApiError::forbidden(
            "registration token is no longer usable (revoked, expired, or exhausted)",
        )
    })?;

    // 4b. Phase 4 — type the advertised capabilities against the workspace's
    //    capability registry BEFORE inserting. The workspace is the (re-read)
    //    registration token's — the same source step 5's INSERT binds. A runner
    //    advertising no caps (`{}`) always passes (empty => Ok). An unknown
    //    capability or a field whose value mismatches its declared FieldKind is
    //    rejected with a 400 carrying the validator's human-readable message.
    //    Returning here drops the open `tx`, rolling back the claimed use so a
    //    bad-caps enroll does not burn a registration-token use.
    let known_caps = load_known_capabilities(&state.db, claimed.workspace_id).await?;
    if let Err(msg) = validate_caps_against_types(&req.capabilities, &known_caps) {
        return Err(ApiError::bad_request(msg));
    }

    // 5. Mint the runner credential + insert the row. workspace_id + group flow
    //    from the (re-read) registration token; enrolled_by = its created_by.
    let runner_id = Uuid::new_v4();
    let minted = mint_token(RUNNER_TOKEN_PREFIX, runner_id);

    let insert = sqlx::query(
        "INSERT INTO runners \
            (id, workspace_id, name, runner_group, token_hash, nats_public_key, capabilities, enrolled_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(runner_id)
    .bind(claimed.workspace_id)
    .bind(req.name.trim())
    .bind(&claimed.group)
    .bind(&minted.token_hash)
    .bind(&req.nats_public_key)
    .bind(&req.capabilities)
    .bind(claimed.created_by)
    .execute(&mut *tx)
    .await;
    if let Err(e) = insert {
        // Dropping the transaction rolls back the claimed use, so a name
        // collision (or any insert failure) doesn't consume the token.
        drop(tx);
        if let Some(db_err) = e.as_database_error() {
            if db_err.is_unique_violation() {
                return Err(ApiError::conflict(format!(
                    "a runner named '{}' already exists in this workspace",
                    req.name.trim()
                )));
            }
        }
        return Err(ApiError::internal(e.to_string()));
    }

    tx.commit().await?;

    // Phase 2 — if the runner sent a NATS user public key, mint a scoped user
    // JWT now so the runner can connect immediately. A mint failure (e.g. a
    // malformed public key) must NOT fail enrollment: log a warning and return
    // `nats_jwt: None`. The runner can always fetch one later via
    // `POST /api/v1/runners/{id}/nats-creds`.
    let nats_jwt = req.nats_public_key.as_deref().and_then(|pubkey| {
        match state
            .runner_nats_signer
            .mint_runner_jwt(pubkey, runner_id, claimed.group.as_deref())
        {
            Ok(jwt) => Some(jwt),
            Err(e) => {
                tracing::warn!(
                    runner_id = %runner_id,
                    error = %e,
                    "could not mint NATS user JWT at enrollment — runner can fetch it later"
                );
                None
            }
        }
    });

    Ok((
        StatusCode::CREATED,
        Json(EnrolledRunner {
            id: runner_id,
            runner_token: minted.full_token,
            workspace_id: claimed.workspace_id,
            group: claimed.group,
            nats_jwt,
        }),
    ))
}

// ── b. Heartbeat (runner-token authed) ─────────────────────────────────────

/// `POST /api/v1/runners/{id}/heartbeat` — bump `last_seen_at`. Authorized by
/// the runner credential: the principal's subject MUST be `runner:{id}` so a
/// runner can only heartbeat itself.
#[utoipa::path(
    post,
    path = "/api/v1/runners/{id}/heartbeat",
    params(("id" = Uuid, Path, description = "Runner id")),
    responses(
        (status = 200, description = "Heartbeat recorded"),
        (status = 401, description = "Wrong / foreign / revoked runner token", body = ErrorResponse),
        (status = 404, description = "Runner not found", body = ErrorResponse),
    ),
    tag = "runners",
)]
pub async fn heartbeat_runner(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    // A runner principal may only heartbeat itself.
    if user.subject != runner_subject(id) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "this token may not heartbeat that runner",
        ));
    }

    let updated =
        sqlx::query("UPDATE runners SET last_seen_at = NOW() WHERE id = $1 AND revoked_at IS NULL")
            .bind(id)
            .execute(&state.db)
            .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found("runner not found"));
    }
    Ok(StatusCode::OK)
}

// ── b2. NATS scoped creds (runner-token authed, self-only) ─────────────────

/// `POST /api/v1/runners/{id}/nats-creds` — mint/rotate the runner's scoped
/// NATS user JWT. Runner-token authed, self-only: the principal's subject MUST
/// be `runner:{id}` (same boundary as heartbeat). The JWT is freshly signed
/// from the runner row's stored `nats_public_key`; 404 if none is stored.
/// Long-lived (no expiry) — calling this again rotates it.
#[utoipa::path(
    post,
    path = "/api/v1/runners/{id}/nats-creds",
    params(("id" = Uuid, Path, description = "Runner id")),
    responses(
        (status = 200, description = "Freshly signed scoped NATS user JWT", body = RunnerNatsCreds),
        (status = 401, description = "Wrong / foreign / revoked runner token", body = ErrorResponse),
        (status = 404, description = "Runner not found or no stored nats_public_key", body = ErrorResponse),
    ),
    tag = "runners",
)]
pub async fn issue_runner_nats_creds(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<RunnerNatsCreds>, ApiError> {
    // A runner principal may only mint creds for itself.
    if user.subject != runner_subject(id) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "this token may not mint creds for that runner",
        ));
    }

    // Load the live runner row (revoked rows are excluded → 404).
    let row = sqlx::query_as::<_, RunnerRow>(
        "SELECT * FROM runners WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("runner not found"))?;

    let pubkey = row
        .nats_public_key
        .as_deref()
        .ok_or_else(|| ApiError::not_found("runner has no stored NATS public key"))?;

    let nats_jwt = state
        .runner_nats_signer
        .mint_runner_jwt(pubkey, id, row.group.as_deref())
        .map_err(|e| ApiError::internal(format!("could not mint NATS user JWT: {e}")))?;

    Ok(Json(RunnerNatsCreds {
        nats_jwt,
        account_public_key: state.runner_nats_signer.account_public_key().to_string(),
    }))
}

// ── b3. Interface catalog (runner-token authed push / human read) ──────────

/// `POST /api/v1/runners/{id}/interfaces` — upsert the runner's self-reported
/// interface catalog (ROS topics/services/actions). Runner-token authed,
/// self-only: the principal's subject MUST be `runner:{id}` (same boundary as
/// heartbeat). One row per runner, keyed on `runner_id`; a repeat push replaces
/// the catalog and bumps `updated_at` while preserving `discovered_at`.
#[utoipa::path(
    post,
    path = "/api/v1/runners/{id}/interfaces",
    params(("id" = Uuid, Path, description = "Runner id")),
    request_body = UpsertRunnerInterfacesRequest,
    responses(
        (status = 204, description = "Interface catalog upserted"),
        (status = 401, description = "Wrong / foreign / revoked runner token", body = ErrorResponse),
        (status = 404, description = "Runner not found", body = ErrorResponse),
    ),
    tag = "runners",
)]
pub async fn upsert_runner_interfaces(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpsertRunnerInterfacesRequest>,
) -> Result<StatusCode, ApiError> {
    // A runner principal may only report interfaces for itself.
    if user.subject != runner_subject(id) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "this token may not report interfaces for that runner",
        ));
    }

    // Resolve the runner's workspace from the live row (revoked rows → 404).
    // This both validates the runner exists and stamps the workspace_id without
    // trusting anything client-supplied.
    let workspace_id: Option<Uuid> =
        sqlx::query_scalar("SELECT workspace_id FROM runners WHERE id = $1 AND revoked_at IS NULL")
            .bind(id)
            .fetch_optional(&state.db)
            .await?;
    let workspace_id = workspace_id.ok_or_else(|| ApiError::not_found("runner not found"))?;

    let catalog = serde_json::to_value(&req.catalog)
        .map_err(|e| ApiError::internal(format!("could not serialize catalog: {e}")))?;

    // Upsert on the runner_id PK: insert a fresh row, or on conflict replace the
    // catalog + version and bump updated_at (discovered_at is preserved).
    sqlx::query(
        "INSERT INTO runner_interfaces \
            (runner_id, workspace_id, catalog, catalog_version) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (runner_id) DO UPDATE SET \
            catalog = EXCLUDED.catalog, \
            catalog_version = EXCLUDED.catalog_version, \
            updated_at = NOW()",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(&catalog)
    .bind(&req.catalog_version)
    .execute(&state.db)
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/v1/runners/{id}/interfaces` — read the runner's interface catalog.
/// Session/human authed + workspace-scoped (same boundary as `get_runner`).
/// 404 when the runner is absent/foreign OR has never pushed a catalog.
#[utoipa::path(
    get,
    path = "/api/v1/runners/{id}/interfaces",
    params(("id" = Uuid, Path, description = "Runner id")),
    responses(
        (status = 200, description = "Runner interface catalog", body = RunnerInterfaces),
        (status = 404, description = "Runner not found or no catalog reported", body = ErrorResponse),
    ),
    tag = "runners",
)]
pub async fn get_runner_interfaces(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<RunnerInterfaces>, ApiError> {
    let workspace_id = caller_workspace(&user);

    // Join through the workspace so a foreign-workspace runner's catalog is never
    // leaked; absence of either the runner-scope match or the catalog row → 404.
    let row: Option<(serde_json::Value, Option<String>, chrono::DateTime<Utc>)> = sqlx::query_as(
        "SELECT ri.catalog, ri.catalog_version, ri.discovered_at \
             FROM runner_interfaces ri \
             WHERE ri.runner_id = $1 AND ri.workspace_id = $2",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await?;

    let (catalog_value, catalog_version, discovered_at) =
        row.ok_or_else(|| ApiError::not_found("no interface catalog reported for this runner"))?;

    let catalog: RunnerInterfaceCatalog = serde_json::from_value(catalog_value)
        .map_err(|e| ApiError::internal(format!("stored catalog is malformed: {e}")))?;

    Ok(Json(RunnerInterfaces {
        runner_id: id,
        catalog,
        catalog_version,
        discovered_at,
    }))
}

// ── c. Management (human) ──────────────────────────────────────────────────

/// `GET /api/v1/runners` — paginated, workspace-scoped (live runners only).
#[utoipa::path(
    get,
    path = "/api/v1/runners",
    params(ListRunnersQuery),
    responses(
        (status = 200, description = "Paginated list of runners", body = PaginatedResponse<RunnerSummary>),
    ),
    tag = "runners",
)]
pub async fn list_runners(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListRunnersQuery>,
) -> Result<Json<PaginatedResponse<RunnerSummary>>, ApiError> {
    let workspace_id = caller_workspace(&user);
    let offset = (params.page - 1) * params.per_page;

    let rows = sqlx::query_as::<_, RunnerRow>(
        "SELECT * FROM runners \
         WHERE workspace_id = $1 AND revoked_at IS NULL \
         ORDER BY enrolled_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(workspace_id)
    .bind(params.per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM runners WHERE workspace_id = $1 AND revoked_at IS NULL",
    )
    .bind(workspace_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(PaginatedResponse {
        items: rows.into_iter().map(RunnerSummary::from).collect(),
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}

/// `GET /api/v1/runners/presence` — live in-memory presence snapshot (Phase 5).
///
/// Returns the presence-controller's in-memory `PresenceMap` — the actual
/// pool-capacity signal (which runners hold an admitted unit right now), NOT the
/// `runners.last_seen_at` column on the list view. Read-only; behind the auth
/// gate like the other management reads.
///
/// The in-memory map is keyed by `runner_id` only and carries no workspace, so
/// it is filtered here against the caller's workspace — a presence row is
/// returned only for a runner that lives in the caller's workspace. Without this
/// the snapshot would leak every workspace's runner ids + liveness timing
/// (tenant-isolation break), since every other runner read is workspace-scoped.
#[utoipa::path(
    get,
    path = "/api/v1/runners/presence",
    responses(
        (status = 200, description = "Live runner presence snapshot", body = [RunnerPresenceSnapshot]),
    ),
    tag = "runners",
)]
pub async fn runner_presence(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<RunnerPresenceSnapshot>>, ApiError> {
    let workspace_id = caller_workspace(&user);
    let own: std::collections::HashSet<Uuid> =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM runners WHERE workspace_id = $1")
            .bind(workspace_id)
            .fetch_all(&state.db)
            .await?
            .into_iter()
            .collect();

    let snapshot = state
        .runner_presence
        .snapshot()
        .await
        .into_iter()
        .filter(|s| own.contains(&s.runner_id))
        .collect();
    Ok(Json(snapshot))
}

/// `GET /api/v1/runners/{id}` — admin view (workspace-scoped).
#[utoipa::path(
    get,
    path = "/api/v1/runners/{id}",
    params(("id" = Uuid, Path, description = "Runner id")),
    responses(
        (status = 200, description = "Runner detail", body = RunnerDetail),
        (status = 404, description = "Runner not found", body = ErrorResponse),
    ),
    tag = "runners",
)]
pub async fn get_runner(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<RunnerDetail>, ApiError> {
    let workspace_id = caller_workspace(&user);
    let row = sqlx::query_as::<_, RunnerRow>(
        "SELECT * FROM runners WHERE id = $1 AND workspace_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("runner not found"))?;
    Ok(Json(RunnerDetail::from(row)))
}

/// `DELETE /api/v1/runners/{id}` — revoke (soft delete + status='revoked').
#[utoipa::path(
    delete,
    path = "/api/v1/runners/{id}",
    params(("id" = Uuid, Path, description = "Runner id")),
    responses(
        (status = 204, description = "Runner revoked"),
        (status = 404, description = "Runner not found", body = ErrorResponse),
    ),
    tag = "runners",
)]
pub async fn revoke_runner(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = caller_workspace(&user);
    let updated = sqlx::query(
        "UPDATE runners SET status = 'revoked', revoked_at = NOW() \
         WHERE id = $1 AND workspace_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(workspace_id)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found("runner not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/runners/registration-tokens` — mint a registration token. The
/// `token` is returned ONCE. Cookie-only (browser human boundary), mirroring
/// `auth_tokens.rs` so a machine token can't mint enrollment secrets.
#[utoipa::path(
    post,
    path = "/api/v1/runners/registration-tokens",
    request_body = CreateRegistrationTokenRequest,
    responses(
        (status = 201, description = "Registration token created (shown once)", body = CreatedRegistrationToken),
        (status = 401, description = "No session", body = ErrorResponse),
    ),
    tag = "runners",
)]
pub async fn create_registration_token(
    State(state): State<AppState>,
    CookieAuthUser(user): CookieAuthUser,
    Json(req): Json<CreateRegistrationTokenRequest>,
) -> Result<(StatusCode, Json<CreatedRegistrationToken>), ApiError> {
    let workspace_id = caller_workspace(&user);
    let created_by = user.subject_as_uuid();
    let reusable = req.reusable.unwrap_or(true);

    // A `max_uses` below 1 mints a token that can never enroll (the use gate is
    // `uses < max_uses`, false from the start) — reject the footgun up front.
    if let Some(max) = req.max_uses {
        if max < 1 {
            return Err(ApiError::bad_request("max_uses must be at least 1"));
        }
    }

    // `group` becomes a literal NATS subject token (`{group}.claim`) in the minted
    // runner JWT. Reject anything that isn't a single safe token so a token
    // creator can't broaden the publish grant via wildcards/extra tokens
    // (`*`, `>`, `.`, whitespace). Defended again in `mint_runner_jwt`.
    if let Some(group) = req.group.as_deref() {
        if !is_safe_group(group) {
            return Err(ApiError::bad_request(
                "group must be a single token of [A-Za-z0-9_-] (no '.', '*', '>', or whitespace)",
            ));
        }
        // A group is only meaningful when BACKED by a presence-backed `capacity`
        // resource (the `instrument` preset) — that resource carries the
        // presence-pool net a runner's unit is admitted into. Reject minting a
        // token for an unbacked group so we never enroll a runner that
        // heartbeats but is admitted to no pool (the silent dangling
        // reference). The operator creates the capacity first; the UI offers
        // that inline.
        if !runner_group_exists(&state.db, workspace_id, group).await? {
            return Err(ApiError::bad_request(format!(
                "no runner group '{group}' exists in this workspace — create a \
                 presence-backed `capacity` resource (the `instrument` preset) \
                 first, then mint a token for it"
            )));
        }
    }

    let reg_id = Uuid::new_v4();
    let minted = mint_token(REG_TOKEN_PREFIX, reg_id);

    sqlx::query(
        "INSERT INTO runner_registration_tokens \
            (id, workspace_id, runner_group, token_hash, reusable, max_uses, expires_at, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(reg_id)
    .bind(workspace_id)
    .bind(&req.group)
    .bind(&minted.token_hash)
    .bind(reusable)
    .bind(req.max_uses)
    .bind(req.expires_at)
    .bind(created_by)
    .execute(&state.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreatedRegistrationToken {
            id: reg_id,
            token: minted.full_token,
            group: req.group,
            reusable,
            max_uses: req.max_uses,
            expires_at: req.expires_at,
        }),
    ))
}

/// `GET /api/v1/runners/registration-tokens` — paginated, workspace-scoped
/// (live tokens only). Never carries the hash.
#[utoipa::path(
    get,
    path = "/api/v1/runners/registration-tokens",
    params(ListRegTokensQuery),
    responses(
        (status = 200, description = "Paginated registration tokens", body = PaginatedResponse<RegistrationTokenSummary>),
    ),
    tag = "runners",
)]
pub async fn list_registration_tokens(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListRegTokensQuery>,
) -> Result<Json<PaginatedResponse<RegistrationTokenSummary>>, ApiError> {
    let workspace_id = caller_workspace(&user);
    let offset = (params.page - 1) * params.per_page;

    let rows = sqlx::query_as::<_, RunnerRegistrationTokenRow>(
        "SELECT * FROM runner_registration_tokens \
         WHERE workspace_id = $1 AND revoked_at IS NULL \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(workspace_id)
    .bind(params.per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM runner_registration_tokens \
         WHERE workspace_id = $1 AND revoked_at IS NULL",
    )
    .bind(workspace_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(PaginatedResponse {
        items: rows
            .into_iter()
            .map(RegistrationTokenSummary::from)
            .collect(),
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}

/// `DELETE /api/v1/runners/registration-tokens/{id}` — revoke a registration
/// token (soft delete; existing runners keep their credentials).
#[utoipa::path(
    delete,
    path = "/api/v1/runners/registration-tokens/{id}",
    params(("id" = Uuid, Path, description = "Registration token id")),
    responses(
        (status = 204, description = "Registration token revoked"),
        (status = 404, description = "Registration token not found", body = ErrorResponse),
    ),
    tag = "runners",
)]
pub async fn revoke_registration_token(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = caller_workspace(&user);
    let updated = sqlx::query(
        "UPDATE runner_registration_tokens SET revoked_at = NOW() \
         WHERE id = $1 AND workspace_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(workspace_id)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found("registration token not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// Handler integration tests require a live DB (`just dev`) — left as TODO:
//   TODO: enroll → heartbeat round-trip (201 + 200) against a seeded reg token.
//   TODO: enroll with revoked/expired/exhausted reg token → 403.
//   TODO: heartbeat with a foreign runner's token → 401.
//   TODO: list/get/revoke workspace-scoping (cross-workspace 404).
// Phase 4 enroll-time capability typing (needs a live DB + a seeded reg token):
//   TODO: define a capability_type (e.g. `xrd` {max_2theta: number, source: text})
//         then enroll with `{"xrd":{"max_2theta":180.0,"source":"synchrotron"}}` → 201.
//   TODO: enroll referencing an UNDEFINED capability (e.g. `{"foo":{}}` in a
//         workspace with no `foo` type) → 400, and assert the reg-token use was
//         NOT consumed (the open tx rolls back on the validation early-return).
//   TODO: enroll with `{}` (no caps) → 201 regardless of the registry.
// Pure token mint/parse/verify unit tests live in `models::runner::tests`.
// Pure `validate_caps_against_types` unit tests live in `models::capability`.
