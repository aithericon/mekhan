//! Humans as a Capacity — the roster HTTP surface (docs/33 §7).
//!
//! The human counterpart to `handlers/runners.rs`. A "human capacity" is a
//! `capacity` resource (`presence · offer · …`) backed by a `pool-<resource_id>`
//! net; the ROSTER is the set of `workspace_members` enrolled into it. These
//! endpoints sit under the `roster` tag and split into two auth shapes:
//!
//!   - **Admin management** (`enroll_member`, `update_roster_member`,
//!     `revoke_roster_member`) — caps are ADMIN-ASSIGNED on the trusted row and
//!     validated against the workspace's `CapabilityType`s exactly like a
//!     runner's enrollment caps (the client never asserts its own).
//!   - **Self service** (`my_enrollments`, `set_availability`) — the caller acts
//!     on their OWN enrollments, keyed on `member_user_id = subject_as_uuid()`.
//!
//! Reads (`list_roster`, `get_roster_member`) are workspace-scoped and live-only
//! (`revoked_at IS NULL`), mirroring `runners`.
//!
//! Route ordering note for the Wire phase: `/roster/me` and
//! `/roster/availability` are LITERAL children of `/roster` and MUST be mounted
//! BEFORE the `/roster/{id}` wildcard, else matchit shadows them.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::models::capability::{load_known_capabilities, validate_caps_against_types};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::roster::{
    AvailabilityRequest, EnrollMemberRequest, RosterMemberDetail, RosterMemberRow,
    RosterMemberSummary, UpdateRosterMemberRequest,
};
use crate::models::template::PaginatedResponse;
use crate::AppState;

/// Caller-implicit workspace: the principal's session workspace, falling back to
/// `Uuid::nil()` for the legacy no-workspace dev shape. Mirrors
/// `runners::caller_workspace`.
fn caller_workspace(user: &AuthUser) -> Uuid {
    user.workspace_id.unwrap_or_else(Uuid::nil)
}

/// Concurrency `C` is the presence controller's slot count — clamp to `>= 1` on
/// every write so an omitted/zero value can never mint a member with no slots.
fn clamp_concurrency(c: Option<u32>) -> i32 {
    c.map(|v| v.max(1)).unwrap_or(1) as i32
}

// ── Query params ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ListRosterQuery {
    /// Optional filter: only members enrolled in this human capacity.
    #[serde(default)]
    pub capacity_id: Option<Uuid>,
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

// ── a. Enroll (admin) ──────────────────────────────────────────────────────

/// `POST /api/v1/roster` — enroll a `workspace_member` into a human capacity.
/// Admin (session `AuthUser`); workspace = caller's. Caps are admin-assigned on
/// the trusted row and validated against the workspace's `CapabilityType`s
/// BEFORE insert — an unknown capability or a value mismatching its declared
/// FieldKind → 400. A repeat enrollment of the same (capacity, member) → 409.
#[utoipa::path(
    post,
    path = "/api/v1/roster",
    request_body = EnrollMemberRequest,
    responses(
        (status = 201, description = "Member enrolled into the human capacity", body = RosterMemberDetail),
        (status = 400, description = "Caps fail validation against the capability registry", body = ErrorResponse),
        (status = 409, description = "Member is already enrolled in this capacity", body = ErrorResponse),
    ),
    tag = "roster",
)]
pub async fn enroll_member(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<EnrollMemberRequest>,
) -> Result<(StatusCode, Json<RosterMemberDetail>), ApiError> {
    let workspace_id = caller_workspace(&user);

    // Type the admin-assigned caps against the workspace's capability registry
    // before inserting — the same gate `runners::enroll_runner` applies. An empty
    // `{}` always passes; an unknown capability or a field whose value mismatches
    // its declared FieldKind → 400 carrying the validator's message.
    let known = load_known_capabilities(&state.db, workspace_id).await?;
    validate_caps_against_types(&req.caps, &known).map_err(ApiError::bad_request)?;

    let concurrency = clamp_concurrency(req.concurrency);
    // An omitted availability config serializes the interactive defaults — never
    // `null` — so the JSONB column always round-trips a typed `AvailabilityConfig`.
    let availability = serde_json::to_value(req.availability.unwrap_or_default())
        .map_err(|e| ApiError::internal(format!("could not serialize availability: {e}")))?;

    let row = sqlx::query_as::<_, RosterMemberRow>(
        "INSERT INTO roster_members \
            (workspace_id, capacity_id, member_user_id, caps, concurrency, availability, available, enrolled_by) \
         VALUES ($1, $2, $3, $4, $5, $6, false, $7) \
         RETURNING *",
    )
    .bind(workspace_id)
    .bind(req.capacity_id)
    .bind(req.member_user_id)
    .bind(&req.caps)
    .bind(concurrency)
    .bind(&availability)
    .bind(user.subject_as_uuid())
    .fetch_one(&state.db)
    .await;

    let row = match row {
        Ok(row) => row,
        Err(e) => {
            if let Some(db_err) = e.as_database_error() {
                if db_err.is_unique_violation() {
                    return Err(ApiError::conflict(
                        "this member is already enrolled in this capacity",
                    ));
                }
            }
            return Err(ApiError::from(e));
        }
    };

    Ok((StatusCode::CREATED, Json(RosterMemberDetail::from(row))))
}

// ── b. List / get (session, workspace-scoped) ──────────────────────────────

/// `GET /api/v1/roster` — paginated, workspace-scoped (live members only).
/// Optionally filtered to a single `capacity_id`.
#[utoipa::path(
    get,
    path = "/api/v1/roster",
    params(ListRosterQuery),
    responses(
        (status = 200, description = "Paginated list of roster members", body = PaginatedResponse<RosterMemberSummary>),
    ),
    tag = "roster",
)]
pub async fn list_roster(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListRosterQuery>,
) -> Result<Json<PaginatedResponse<RosterMemberSummary>>, ApiError> {
    let workspace_id = caller_workspace(&user);
    let offset = (params.page - 1) * params.per_page;

    // `capacity_id` is an optional filter: a NULL bind makes the
    // `($N IS NULL OR capacity_id = $N)` clause match every row.
    let rows = sqlx::query_as::<_, RosterMemberRow>(
        "SELECT * FROM roster_members \
         WHERE workspace_id = $1 AND revoked_at IS NULL \
           AND ($2::uuid IS NULL OR capacity_id = $2) \
         ORDER BY enrolled_at DESC LIMIT $3 OFFSET $4",
    )
    .bind(workspace_id)
    .bind(params.capacity_id)
    .bind(params.per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM roster_members \
         WHERE workspace_id = $1 AND revoked_at IS NULL \
           AND ($2::uuid IS NULL OR capacity_id = $2)",
    )
    .bind(workspace_id)
    .bind(params.capacity_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(PaginatedResponse {
        items: rows.into_iter().map(RosterMemberSummary::from).collect(),
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}

/// `GET /api/v1/roster/me` — the caller's OWN live enrollments across the
/// workspace. Self-service read keyed on `member_user_id = subject_as_uuid()`;
/// feeds the availability UI. Returns the full [`RosterMemberDetail`] (caps +
/// typed availability) since a member is trusted to see their own enrollment.
///
/// Mounted BEFORE `/roster/{id}` so matchit routes `me` to this literal handler.
#[utoipa::path(
    get,
    path = "/api/v1/roster/me",
    responses(
        (status = 200, description = "The caller's own live roster enrollments", body = [RosterMemberDetail]),
    ),
    tag = "roster",
)]
pub async fn my_enrollments(
    State(state): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<RosterMemberDetail>>, ApiError> {
    let workspace_id = caller_workspace(&user);
    let member = user.subject_as_uuid();

    let rows = sqlx::query_as::<_, RosterMemberRow>(
        "SELECT * FROM roster_members \
         WHERE workspace_id = $1 AND member_user_id = $2 AND revoked_at IS NULL \
         ORDER BY enrolled_at DESC",
    )
    .bind(workspace_id)
    .bind(member)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(rows.into_iter().map(RosterMemberDetail::from).collect()))
}

/// `GET /api/v1/roster/{id}` — admin view of a single member (workspace-scoped).
/// 404 when missing or revoked.
#[utoipa::path(
    get,
    path = "/api/v1/roster/{id}",
    params(("id" = Uuid, Path, description = "Roster member id")),
    responses(
        (status = 200, description = "Roster member detail", body = RosterMemberDetail),
        (status = 404, description = "Roster member not found", body = ErrorResponse),
    ),
    tag = "roster",
)]
pub async fn get_roster_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<RosterMemberDetail>, ApiError> {
    let workspace_id = caller_workspace(&user);
    let row = sqlx::query_as::<_, RosterMemberRow>(
        "SELECT * FROM roster_members WHERE id = $1 AND workspace_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("roster member not found"))?;
    Ok(Json(RosterMemberDetail::from(row)))
}

// ── c. Update / revoke (admin, workspace-scoped) ───────────────────────────

/// `PATCH /api/v1/roster/{id}` — admin update of a member's caps / concurrency /
/// availability. Every field optional; only the supplied ones are written. When
/// `caps` is supplied it is re-validated against the workspace's
/// `CapabilityType`s (same gate as enroll). 404 when missing or revoked.
#[utoipa::path(
    patch,
    path = "/api/v1/roster/{id}",
    params(("id" = Uuid, Path, description = "Roster member id")),
    request_body = UpdateRosterMemberRequest,
    responses(
        (status = 200, description = "Updated roster member detail", body = RosterMemberDetail),
        (status = 400, description = "Caps fail validation against the capability registry", body = ErrorResponse),
        (status = 404, description = "Roster member not found", body = ErrorResponse),
    ),
    tag = "roster",
)]
pub async fn update_roster_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateRosterMemberRequest>,
) -> Result<Json<RosterMemberDetail>, ApiError> {
    let workspace_id = caller_workspace(&user);

    // Re-validate caps against the registry whenever they're being written — a
    // PATCH must not be a back door around the enroll-time typing gate.
    if let Some(caps) = req.caps.as_ref() {
        let known = load_known_capabilities(&state.db, workspace_id).await?;
        validate_caps_against_types(caps, &known).map_err(ApiError::bad_request)?;
    }

    // A supplied availability config is serialized to JSONB; an omitted one leaves
    // the stored column untouched (the COALESCE keeps the existing value).
    let availability = match req.availability {
        Some(cfg) => Some(
            serde_json::to_value(cfg)
                .map_err(|e| ApiError::internal(format!("could not serialize availability: {e}")))?,
        ),
        None => None,
    };
    let concurrency = req.concurrency.map(|c| c.max(1) as i32);

    let row = sqlx::query_as::<_, RosterMemberRow>(
        "UPDATE roster_members SET \
            caps = COALESCE($3, caps), \
            concurrency = COALESCE($4, concurrency), \
            availability = COALESCE($5, availability) \
         WHERE id = $1 AND workspace_id = $2 AND revoked_at IS NULL \
         RETURNING *",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(req.caps)
    .bind(concurrency)
    .bind(availability)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("roster member not found"))?;

    Ok(Json(RosterMemberDetail::from(row)))
}

/// `DELETE /api/v1/roster/{id}` — revoke a member (soft delete; sets
/// `revoked_at`). Workspace-scoped; 404 when missing or already revoked.
#[utoipa::path(
    delete,
    path = "/api/v1/roster/{id}",
    params(("id" = Uuid, Path, description = "Roster member id")),
    responses(
        (status = 204, description = "Roster member revoked"),
        (status = 404, description = "Roster member not found", body = ErrorResponse),
    ),
    tag = "roster",
)]
pub async fn revoke_roster_member(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = caller_workspace(&user);
    let updated = sqlx::query(
        "UPDATE roster_members SET revoked_at = NOW() \
         WHERE id = $1 AND workspace_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(workspace_id)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found("roster member not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── d. Self-service availability ───────────────────────────────────────────

/// `POST /api/v1/roster/availability` — the caller flips their OWN durable
/// availability intent on a specific human capacity. Self-service: keyed on
/// `member_user_id = subject_as_uuid()`, so a member can only toggle their own
/// presence. 404 when the caller is not enrolled in that capacity.
///
/// The durable `available` row is the source of truth; AFTER the commit we
/// publish a fire-and-forget CORE NATS message so the presence controller learns
/// the intent edge live (`human.{member}.availability`). A publish failure is
/// warned-and-swallowed — the next reconcile reads the durable row.
///
/// Mounted BEFORE `/roster/{id}` so matchit routes `availability` to this literal
/// handler.
#[utoipa::path(
    post,
    path = "/api/v1/roster/availability",
    request_body = AvailabilityRequest,
    responses(
        (status = 200, description = "Availability intent recorded"),
        (status = 404, description = "Caller is not enrolled in that capacity", body = ErrorResponse),
    ),
    tag = "roster",
)]
pub async fn set_availability(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<AvailabilityRequest>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = caller_workspace(&user);
    let member = user.subject_as_uuid();

    // Flip the durable intent on the caller's own enrollment. `available_since`
    // tracks the rising edge: NOW() when going online, NULL when going offline.
    let updated = sqlx::query(
        "UPDATE roster_members SET \
            available = $4, \
            available_since = (CASE WHEN $4 THEN NOW() ELSE NULL END) \
         WHERE workspace_id = $1 AND capacity_id = $2 AND member_user_id = $3 \
           AND revoked_at IS NULL",
    )
    .bind(workspace_id)
    .bind(req.capacity_id)
    .bind(member)
    .bind(req.available)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found(
            "you are not enrolled in that capacity",
        ));
    }

    // The durable row above is the source of truth; this publish is the live edge
    // the presence controller wakes on. Fire-and-forget on the CORE client, warn
    // and swallow on any transport error (the next reconcile reads the row).
    let subject = format!("human.{member}.availability");
    let payload = serde_json::json!({
        "available": req.available,
        "capacity_id": req.capacity_id.to_string(),
        "workspace_id": workspace_id.to_string(),
    });
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    if let Err(e) = state.nats.client().publish(subject, bytes.into()).await {
        tracing::warn!(
            %member,
            capacity_id = %req.capacity_id,
            "could not publish availability intent — durable row is the source of truth: {e}"
        );
    }

    Ok(StatusCode::OK)
}
