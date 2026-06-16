//! Worker-pool coverage endpoint.
//!
//! The worker pool is a set of anonymous, competing-consumer executor workers
//! (NOT enrolled runners — see [`crate::handlers::runners`] for the
//! presence-pool / instrument path). Each worker advertises which
//! `ExecutorJob` backends it serves via `worker.<id>.presence`;
//! [`crate::fleet`] tracks that as advisory, TTL-swept presence (the worker
//! facet of the unified fleet-liveness registry).
//!
//! This read surfaces that map so an operator can see the live pool: which
//! workers are connected and, crucially, which backends are covered by ZERO
//! live workers (a step on such a backend will queue at `submitted` until a
//! worker connects). The per-backend list enumerates EVERY `ExecutorJob`
//! backend — a `worker_count` of 0 is the actionable signal.
//!
//! Read-only, behind the auth gate like the other management reads. The pool is
//! shared infrastructure with no workspace, so — unlike the workspace-scoped
//! runner reads — coverage is global (not filtered per tenant).

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::extractor::CookieAuthUser;
use crate::auth::worker_token::worker_subject;
use crate::auth::AuthUser;
use crate::models::asset::PLATFORM_SCOPE_ID;
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::template::PaginatedResponse;
use crate::models::worker::{
    CreateWorkerRegistrationTokenRequest, CreatedWorkerRegistrationToken, EnrollWorkerRequest,
    EnrolledWorker, WorkerDetail, WorkerNatsCreds, WorkerRegistrationTokenRow,
    WorkerRegistrationTokenSummary, WorkerRow, WorkerSummary, WORKER_REG_TOKEN_PREFIX,
    WORKER_TOKEN_PREFIX,
};
use crate::AppState;

/// One live worker's advertised coverage.
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkerCoverageEntry {
    /// Self-reported worker id (the executor daemon's name).
    pub worker_id: String,
    /// `ExecutorJob` backend wire names this worker serves (e.g. `python`).
    pub backends: Vec<String>,
    /// Milliseconds since this worker's last presence heartbeat.
    pub last_seen_ms_ago: u64,
}

/// Per-backend coverage across every `ExecutorJob` backend. A `worker_count` of
/// 0 means NO live worker serves this backend — steps on it will queue.
#[derive(Debug, Serialize, ToSchema)]
pub struct BackendCoverageEntry {
    /// Snake-case backend wire name (`python`, `loki`, …).
    pub backend: String,
    /// Human label for the backend (editor display name).
    pub display_name: String,
    /// Number of live workers advertising this backend.
    pub worker_count: u32,
}

/// Worker-pool coverage snapshot: live workers + per-backend coverage.
#[derive(Debug, Serialize, ToSchema)]
pub struct WorkerCoverageResponse {
    /// Live workers (TTL-swept), each with its advertised backends + freshness.
    pub workers: Vec<WorkerCoverageEntry>,
    /// Coverage for EVERY `ExecutorJob` backend; `worker_count == 0` is uncovered.
    pub backends: Vec<BackendCoverageEntry>,
}

/// `GET /api/v1/workers/coverage` — live worker-pool coverage.
///
/// Reads the in-memory presence map populated from `worker.*.presence`. Global
/// (the pool has no workspace); behind the auth gate like the other reads.
#[utoipa::path(
    get,
    path = "/api/v1/workers/coverage",
    responses(
        (status = 200, description = "Live worker-pool coverage snapshot", body = WorkerCoverageResponse),
    ),
    tag = "workers",
)]
pub async fn worker_coverage(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<WorkerCoverageResponse>, ApiError> {
    // Filter the unified fleet snapshot to the WORKER facet — this endpoint is
    // the anonymous worker-pool coverage view (runners have their own presence
    // read), so a mirrored runner entry must not appear here or inflate counts.
    let snapshot: Vec<crate::fleet::FleetSnapshotEntry> = state
        .fleet
        .snapshot()
        .await
        .into_iter()
        .filter(|e| matches!(e.kind, crate::fleet::CapacityKind::Worker))
        .collect();

    let workers: Vec<WorkerCoverageEntry> = snapshot
        .iter()
        .map(|e| WorkerCoverageEntry {
            worker_id: e.id.clone(),
            backends: e.caps.clone(),
            last_seen_ms_ago: e.last_seen_ms_ago,
        })
        .collect();

    // Enumerate EVERY ExecutorJob backend (not just covered ones) so the UI can
    // surface uncovered backends (worker_count == 0) — the actionable signal.
    let backends: Vec<BackendCoverageEntry> = aithericon_backends::BACKENDS
        .iter()
        .filter(|m| {
            matches!(
                m.dispatch_mode,
                aithericon_backends::DispatchMode::ExecutorJob
            )
        })
        .map(|m| {
            let worker_count = snapshot
                .iter()
                .filter(|e| e.caps.iter().any(|b| b == m.wire_name))
                .count() as u32;
            BackendCoverageEntry {
                backend: m.wire_name.to_string(),
                display_name: m.display_name.to_string(),
                worker_count,
            }
        })
        .collect();

    Ok(Json(WorkerCoverageResponse { workers, backends }))
}

// ─────────────────────────────────────────────────────────────────────────────
// Identity plane — enrolled, group-scoped, revocable workers (Phase A, docs/23
// + docs/24). The exact parallel of [`crate::handlers::runners`], reusing the
// prefix-parameterized token helpers; only the auth subject (`worker:{id}`), the
// table (`workers`), and the group-backed gate (the `capacity` WORKER preset,
// NOT `runner_group`) differ.
// ─────────────────────────────────────────────────────────────────────────────

/// Caller-implicit workspace: the principal's session workspace, or 403 when
/// the caller has no active workspace (no silent nil-tenant fallback). Mirrors
/// `runners::caller_workspace`.
fn caller_workspace(user: &AuthUser) -> Result<Uuid, ApiError> {
    user.require_workspace()
}

/// A `group` alias is interpolated verbatim into a NATS subject (the group's pull
/// filter `executor-<wire>.*.<group>.>`) in the minted worker JWT, so it must be a
/// single safe subject token — no `.` (extra tokens), `*`/`>` (wildcards), or
/// whitespace that would broaden the granted SUBSCRIBE permission. Empty is
/// rejected here; callers treat `None`/empty as "no group" before this point.
/// Identical discipline to `runners::is_safe_group`.
fn is_safe_group(group: &str) -> bool {
    !group.is_empty()
        && group
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Whether a live `capacity` resource with the WORKER preset named `alias` exists
/// in `workspace_id`.
///
/// A worker's `group` is only meaningful if it is BACKED by a `capacity` resource
/// sitting at the worker point in the trait-space: `competing_consumer` liveness +
/// `auto` acceptance (the `worker` preset — see `models/capacity.rs::presets`). That
/// is the named partition coordinate the group routes onto; without it, an
/// enrolled worker would carry a group token that addresses no declared pool — a
/// silent dangling reference. We forbid minting a registration token for an
/// unbacked group so the dangling reference can't be created at its source; the
/// operator must create the group (a `capacity` resource, `worker` preset) first.
///
/// This is the WORKER analogue of `runners::runner_group_exists`, but it gates on
/// `resource_type='capacity'` + the worker axes rather than `runner_group`: a
/// presence/lease (instrument/HPC) capacity is the PUSH path and is NOT a valid
/// worker group. The axes live in the latest version's `public_config`, so this
/// joins `resources` → `resource_versions` at `latest_version` and matches the two
/// worker-defining axis strings (`liveness` / `acceptance`).
async fn worker_group_exists(
    db: &sqlx::PgPool,
    workspace_id: Uuid,
    alias: &str,
) -> Result<bool, ApiError> {
    // A token (or worker) carrying `workspace_id = PLATFORM_SCOPE_ID` addresses
    // the shared platform-tier worker pool, whose backing `capacity` row is
    // `scope_kind = 'platform'`. Resolve it against the platform scope so the
    // enroll/mint gates recognise the platform `default` group.
    if workspace_id == PLATFORM_SCOPE_ID {
        return Ok(
            crate::worker_groups::resolve_platform_default_worker_group_uuid(db)
                .await?
                .is_some(),
        );
    }
    let found: Option<(Uuid,)> = sqlx::query_as::<_, (Uuid,)>(
        "SELECT r.id FROM resources r \
         JOIN resource_versions rv \
           ON rv.resource_id = r.id AND rv.version = r.latest_version \
         WHERE r.workspace_id = $1 AND r.path = $2 \
           AND r.resource_type = 'capacity' AND r.deleted_at IS NULL \
           AND rv.public_config ->> 'liveness' = 'competing_consumer' \
           AND rv.public_config ->> 'acceptance' = 'auto'",
    )
    .bind(workspace_id)
    .bind(alias)
    .fetch_optional(db)
    .await?;
    Ok(found.is_some())
}

/// Build the live-presence overlay from the in-memory [`crate::fleet`] snapshot:
/// a map from each currently-live WORKER's id (the `wkr_` UUID it presence-reports
/// on `worker.{id}.presence`) to its last-heartbeat timestamp (derived from the
/// snapshot's relative age). A worker present in this map is `online`; its mapped
/// timestamp is fresher than (or equal to) the persisted `last_seen_at`.
///
/// This is the seam that makes the worker reads reflect the signal the executor
/// actually emits (NATS presence), rather than the never-called HTTP heartbeat.
/// Runner entries are filtered out — the worker reads are the worker-pool view.
async fn live_worker_overlay(state: &AppState) -> std::collections::HashMap<Uuid, DateTime<Utc>> {
    let now = Utc::now();
    state
        .fleet
        .snapshot()
        .await
        .into_iter()
        .filter(|e| matches!(e.kind, crate::fleet::CapacityKind::Worker))
        .filter_map(|e| {
            // Only enrolled workers (UUID ids) correlate to a DB row; an anonymous
            // worker's process-name id has no row to overlay.
            let id = Uuid::parse_str(&e.id).ok()?;
            let last_seen = now - chrono::Duration::milliseconds(e.last_seen_ms_ago as i64);
            Some((id, last_seen))
        })
        .collect()
}

/// Overlay one DB-projected [`WorkerSummary`] with live presence: set `online` and
/// (when live) bump `last_seen_at` to the heartbeat-derived timestamp.
fn apply_overlay(
    mut w: WorkerSummary,
    overlay: &std::collections::HashMap<Uuid, DateTime<Utc>>,
) -> WorkerSummary {
    if let Some(live_seen) = overlay.get(&w.id) {
        w.online = true;
        w.last_seen_at = Some(*live_seen);
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(id: Uuid, last_seen: Option<DateTime<Utc>>) -> WorkerSummary {
        WorkerSummary {
            id,
            name: "w".to_string(),
            group: None,
            routing_partition: Uuid::nil(),
            status: "enrolled".to_string(),
            backends: serde_json::json!([]),
            online: false,
            last_seen_at: last_seen,
            enrolled_at: Utc::now(),
        }
    }

    #[test]
    fn overlay_marks_live_worker_online_and_freshens_last_seen() {
        let id = Uuid::new_v4();
        let live = Utc::now();
        let overlay = std::collections::HashMap::from([(id, live)]);

        // A worker with a STALE persisted last_seen that is currently live: the
        // overlay flips `online` and replaces last_seen with the live timestamp.
        let stale = Utc::now() - chrono::Duration::hours(2);
        let out = apply_overlay(summary(id, Some(stale)), &overlay);
        assert!(out.online, "present in snapshot ⇒ online");
        assert_eq!(out.last_seen_at, Some(live), "last_seen overlaid from snapshot");
    }

    #[test]
    fn overlay_leaves_absent_worker_offline_with_persisted_last_seen() {
        // A worker NOT in the snapshot stays offline; its persisted last_seen
        // (bridged from earlier presence) is preserved so the UI can still show
        // "last seen 5m ago" for a worker that has since gone away.
        let persisted = Utc::now() - chrono::Duration::minutes(5);
        let out = apply_overlay(summary(Uuid::new_v4(), Some(persisted)), &std::collections::HashMap::new());
        assert!(!out.online, "absent from snapshot ⇒ offline");
        assert_eq!(out.last_seen_at, Some(persisted), "persisted last_seen kept");
    }
}

// ── Query params ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ListWorkersQuery {
    #[serde(default = "default_page")]
    pub page: i64,
    #[serde(default = "default_per_page")]
    pub per_page: i64,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct ListWorkerRegTokensQuery {
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

/// `POST /api/v1/workers/enroll` — GitLab-style enrollment. PUBLIC: authed by the
/// `wt_` token in the body. The enrolled worker inherits the registration token's
/// `workspace_id` + `group`; `enrolled_by` is the token's `created_by`. Mints the
/// `wkr_` credential (shown once) and, if a NATS public key was supplied, a scoped
/// worker JWT.
#[utoipa::path(
    post,
    path = "/api/v1/workers/enroll",
    request_body = EnrollWorkerRequest,
    responses(
        (status = 201, description = "Worker enrolled (worker_token shown once)", body = EnrolledWorker),
        (status = 401, description = "Invalid registration token", body = ErrorResponse),
        (status = 403, description = "Revoked / expired / exhausted registration token", body = ErrorResponse),
        (status = 409, description = "Worker name already exists in workspace", body = ErrorResponse),
    ),
    tag = "workers",
)]
pub async fn enroll_worker(
    State(state): State<AppState>,
    Json(req): Json<EnrollWorkerRequest>,
) -> Result<(StatusCode, Json<EnrolledWorker>), ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::bad_request("worker name must not be empty"));
    }

    // 1. Parse + look up the registration token row by its uuid prefix.
    let (reg_id, secret) =
        crate::models::runner::parse_token(WORKER_REG_TOKEN_PREFIX, &req.registration_token)
            .ok_or_else(|| {
                ApiError::new(StatusCode::UNAUTHORIZED, "malformed registration token")
            })?;

    let reg = sqlx::query_as::<_, WorkerRegistrationTokenRow>(
        "SELECT * FROM worker_registration_tokens WHERE id = $1",
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

    // 4. Atomically claim a use of the registration token. This guarded UPDATE —
    //    not the friendly checks above — is the real authorization gate: the
    //    checks at step 3 are read-then-act and thus racy, so the WHERE here
    //    re-validates every condition while incrementing. Two concurrent enrolls
    //    of a single-use / near-`max_uses` token therefore cannot both succeed:
    //    at most one UPDATE matches and bumps `uses`. The secret was already
    //    verified constant-time against the fetched row above; the whole
    //    claim+insert runs in one transaction so a worker-name collision rolls
    //    the claimed use back instead of burning it.
    let mut tx = state.db.begin().await?;

    let claimed = sqlx::query_as::<_, WorkerRegistrationTokenRow>(
        "UPDATE worker_registration_tokens \
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

    // 5. Resolve the ROUTING PARTITION (unified worker dispatch, docs/23/24): the
    //    worker competes in a group whose partition token is that group's
    //    `capacity`-resource UUID, NOT its alias. The token's `group` names the
    //    group; a token that names none inherits the workspace's always-seeded
    //    `default` group. We resolve the alias → UUID here (the `default` group is
    //    guaranteed by the startup seeder + the migration backfill). The human
    //    alias stays in `worker_group` for display; the UUID is the routing key
    //    the executor binds + the JWT subscribe filter uses.
    let group_alias = claimed
        .group
        .as_deref()
        .filter(|g| !g.is_empty())
        .unwrap_or(crate::worker_groups::DEFAULT_WORKER_GROUP_PATH);
    // A platform-tier registration token (`workspace_id = PLATFORM_SCOPE_ID`,
    // group `default`) enrols the worker into the shared platform pool — resolve
    // its routing partition against the platform-scoped group. (Explicit
    // non-default groups on a platform token are not minted; the mint gate only
    // backs the platform `default`.)
    let routing_partition = if claimed.workspace_id == PLATFORM_SCOPE_ID {
        crate::worker_groups::resolve_platform_default_worker_group_uuid(&state.db).await?
    } else {
        crate::worker_groups::resolve_worker_group_uuid(
            &state.db,
            claimed.workspace_id,
            group_alias,
        )
        .await?
    }
    .ok_or_else(|| {
        // Should never fire: the default group is always seeded, and an explicit
        // group is gated at registration-token mint (`worker_group_exists`).
        ApiError::bad_request(format!(
            "worker group '{group_alias}' does not resolve to a worker `capacity` \
             resource in this workspace"
        ))
    })?;

    // 6. Mint the worker credential + insert the row. workspace_id + group +
    //    routing_partition flow from the (re-read) registration token + the
    //    resolution above; enrolled_by = its created_by. The advertised
    //    `backends` come from the enroll body (JSON array column).
    let worker_id = Uuid::new_v4();
    let minted = crate::models::runner::mint_token(WORKER_TOKEN_PREFIX, worker_id);
    let backends_json = serde_json::json!(req.backends);

    let insert = sqlx::query(
        "INSERT INTO workers \
            (id, workspace_id, name, worker_group, routing_partition, token_hash, nats_public_key, backends, enrolled_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(worker_id)
    .bind(claimed.workspace_id)
    .bind(req.name.trim())
    .bind(&claimed.group)
    .bind(routing_partition)
    .bind(&minted.token_hash)
    .bind(&req.nats_public_key)
    .bind(&backends_json)
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
                    "a worker named '{}' already exists in this workspace",
                    req.name.trim()
                )));
            }
        }
        return Err(ApiError::internal(e.to_string()));
    }

    tx.commit().await?;

    // If the worker sent a NATS user public key, mint a scoped user JWT now so it
    // can connect immediately. A mint failure (e.g. a malformed public key) must
    // NOT fail enrollment: log a warning and return `nats_jwt: None`. The worker
    // can always fetch one later via `POST /api/v1/workers/{id}/nats-creds`.
    // The JWT's group pull filter is the ROUTING PARTITION (UUID), not the alias:
    // the worker subscribes to `executor-<wire>-grp.*.<routing_partition>.>`.
    let partition_token = routing_partition.to_string();
    let nats_jwt = req.nats_public_key.as_deref().and_then(|pubkey| {
        match state.runner_nats_signer.mint_worker_jwt(
            pubkey,
            worker_id,
            Some(partition_token.as_str()),
            &req.backends,
        ) {
            Ok(jwt) => Some(jwt),
            Err(e) => {
                tracing::warn!(
                    worker_id = %worker_id,
                    error = %e,
                    "could not mint NATS user JWT at enrollment — worker can fetch it later"
                );
                None
            }
        }
    });

    Ok((
        StatusCode::CREATED,
        Json(EnrolledWorker {
            id: worker_id,
            worker_token: minted.full_token,
            workspace_id: claimed.workspace_id,
            group: claimed.group,
            routing_partition,
            nats_jwt,
        }),
    ))
}

// ── b. Heartbeat (worker-token authed, self-only) ──────────────────────────

/// `POST /api/v1/workers/{id}/heartbeat` — bump `last_seen_at`. Authorized by the
/// worker credential: the principal's subject MUST be `worker:{id}` so a worker
/// can only heartbeat itself.
#[utoipa::path(
    post,
    path = "/api/v1/workers/{id}/heartbeat",
    params(("id" = Uuid, Path, description = "Worker id")),
    responses(
        (status = 200, description = "Heartbeat recorded"),
        (status = 401, description = "Wrong / foreign / revoked worker token", body = ErrorResponse),
        (status = 404, description = "Worker not found", body = ErrorResponse),
    ),
    tag = "workers",
)]
pub async fn heartbeat_worker(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    // A worker principal may only heartbeat itself.
    if user.subject != worker_subject(id) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "this token may not heartbeat that worker",
        ));
    }

    let updated =
        sqlx::query("UPDATE workers SET last_seen_at = NOW() WHERE id = $1 AND revoked_at IS NULL")
            .bind(id)
            .execute(&state.db)
            .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found("worker not found"));
    }
    Ok(StatusCode::OK)
}

// ── b2. NATS scoped creds (worker-token authed, self-only) ─────────────────

/// `POST /api/v1/workers/{id}/nats-creds` — mint/rotate the worker's scoped NATS
/// user JWT. Worker-token authed, self-only: the principal's subject MUST be
/// `worker:{id}` (same boundary as heartbeat). The JWT is freshly signed from the
/// worker row's stored `nats_public_key` + advertised backends; 404 if no public
/// key is stored. Long-lived (no expiry) — calling this again rotates it.
#[utoipa::path(
    post,
    path = "/api/v1/workers/{id}/nats-creds",
    params(("id" = Uuid, Path, description = "Worker id")),
    responses(
        (status = 200, description = "Freshly signed scoped NATS user JWT", body = WorkerNatsCreds),
        (status = 401, description = "Wrong / foreign / revoked worker token", body = ErrorResponse),
        (status = 404, description = "Worker not found or no stored nats_public_key", body = ErrorResponse),
    ),
    tag = "workers",
)]
pub async fn issue_worker_nats_creds(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkerNatsCreds>, ApiError> {
    // A worker principal may only mint creds for itself.
    if user.subject != worker_subject(id) {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "this token may not mint creds for that worker",
        ));
    }

    // Load the live worker row (revoked rows are excluded → 404).
    let row = sqlx::query_as::<_, WorkerRow>(
        "SELECT * FROM workers WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("worker not found"))?;

    let pubkey = row
        .nats_public_key
        .as_deref()
        .ok_or_else(|| ApiError::not_found("worker has no stored NATS public key"))?;

    // The advertised backends are stored as a JSON array; the scope's SUBSCRIBE
    // grant is built per backend wire, so decode them back to a `Vec<String>`
    // (treating a malformed / non-array blob as "no backends").
    let backends: Vec<String> = serde_json::from_value(row.backends.clone()).unwrap_or_default();

    // The pull filter is the ROUTING PARTITION (UUID), not the human alias —
    // `executor-<wire>-grp.*.<routing_partition>.>`.
    let partition_token = row.routing_partition.to_string();
    let nats_jwt = state
        .runner_nats_signer
        .mint_worker_jwt(pubkey, id, Some(partition_token.as_str()), &backends)
        .map_err(|e| ApiError::internal(format!("could not mint NATS user JWT: {e}")))?;

    Ok(Json(WorkerNatsCreds {
        nats_jwt,
        account_public_key: state.runner_nats_signer.account_public_key().to_string(),
    }))
}

// ── c. Management (human) ──────────────────────────────────────────────────

/// `GET /api/v1/workers` — paginated, workspace-scoped (live workers only).
#[utoipa::path(
    get,
    path = "/api/v1/workers",
    params(ListWorkersQuery),
    responses(
        (status = 200, description = "Paginated list of workers", body = PaginatedResponse<WorkerSummary>),
    ),
    tag = "workers",
)]
pub async fn list_workers(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListWorkersQuery>,
) -> Result<Json<PaginatedResponse<WorkerSummary>>, ApiError> {
    let workspace_id = caller_workspace(&user)?;
    let offset = (params.page - 1) * params.per_page;

    let rows = sqlx::query_as::<_, WorkerRow>(
        "SELECT * FROM workers \
         WHERE workspace_id = $1 AND revoked_at IS NULL \
         ORDER BY enrolled_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(workspace_id)
    .bind(params.per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM workers WHERE workspace_id = $1 AND revoked_at IS NULL",
    )
    .bind(workspace_id)
    .fetch_one(&state.db)
    .await?;

    let overlay = live_worker_overlay(&state).await;

    Ok(Json(PaginatedResponse {
        items: rows
            .into_iter()
            .map(|r| apply_overlay(WorkerSummary::from(r), &overlay))
            .collect(),
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}

/// `GET /api/v1/workers/{id}` — admin view (workspace-scoped).
#[utoipa::path(
    get,
    path = "/api/v1/workers/{id}",
    params(("id" = Uuid, Path, description = "Worker id")),
    responses(
        (status = 200, description = "Worker detail", body = WorkerDetail),
        (status = 404, description = "Worker not found", body = ErrorResponse),
    ),
    tag = "workers",
)]
pub async fn get_worker(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<WorkerDetail>, ApiError> {
    let workspace_id = caller_workspace(&user)?;
    let row = sqlx::query_as::<_, WorkerRow>(
        "SELECT * FROM workers WHERE id = $1 AND workspace_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(workspace_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| ApiError::not_found("worker not found"))?;

    // Overlay live presence from the in-memory fleet snapshot (same seam as the
    // list view): set `online` and, when live, the heartbeat-derived `last_seen_at`.
    let mut detail = WorkerDetail::from(row);
    let overlay = live_worker_overlay(&state).await;
    if let Some(live_seen) = overlay.get(&detail.id) {
        detail.online = true;
        detail.last_seen_at = Some(*live_seen);
    }
    Ok(Json(detail))
}

/// `DELETE /api/v1/workers/{id}` — revoke (soft delete + status='revoked'). D2
/// revocation: immediate control-plane lockout (heartbeat/creds 401) + future
/// enroll blocked. The live NATS connection is not booted (deferred follow-up).
#[utoipa::path(
    delete,
    path = "/api/v1/workers/{id}",
    params(("id" = Uuid, Path, description = "Worker id")),
    responses(
        (status = 204, description = "Worker revoked"),
        (status = 404, description = "Worker not found", body = ErrorResponse),
    ),
    tag = "workers",
)]
pub async fn revoke_worker(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = caller_workspace(&user)?;
    let updated = sqlx::query(
        "UPDATE workers SET status = 'revoked', revoked_at = NOW() \
         WHERE id = $1 AND workspace_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(workspace_id)
    .execute(&state.db)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found("worker not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/workers/registration-tokens` — mint a registration token. The
/// `token` is returned ONCE. Cookie-only (browser human boundary), mirroring
/// `runners`/`auth_tokens` so a machine token can't mint enrollment secrets.
#[utoipa::path(
    post,
    path = "/api/v1/workers/registration-tokens",
    request_body = CreateWorkerRegistrationTokenRequest,
    responses(
        (status = 201, description = "Registration token created (shown once)", body = CreatedWorkerRegistrationToken),
        (status = 401, description = "No session", body = ErrorResponse),
    ),
    tag = "workers",
)]
pub async fn create_worker_registration_token(
    State(state): State<AppState>,
    CookieAuthUser(user): CookieAuthUser,
    Json(req): Json<CreateWorkerRegistrationTokenRequest>,
) -> Result<(StatusCode, Json<CreatedWorkerRegistrationToken>), ApiError> {
    // A platform-tier token enrols workers into the shared global pool — curation
    // is a platform-admin capability, so gate the mint on `is_platform_admin` and
    // force the token's `workspace_id` to the platform sentinel. A normal token
    // is workspace-scoped to the caller's session workspace.
    let workspace_id = if req.platform {
        if !user.is_platform_admin {
            return Err(ApiError::forbidden(
                "minting a platform worker registration token requires platform admin",
            ));
        }
        PLATFORM_SCOPE_ID
    } else {
        caller_workspace(&user)?
    };
    let created_by = user.subject_as_uuid();
    let reusable = req.reusable.unwrap_or(true);

    // A `max_uses` below 1 mints a token that can never enroll (the use gate is
    // `uses < max_uses`, false from the start) — reject the footgun up front.
    if let Some(max) = req.max_uses {
        if max < 1 {
            return Err(ApiError::bad_request("max_uses must be at least 1"));
        }
    }

    // `group` becomes a literal NATS subject token (the group's pull filter) in
    // the minted worker JWT. Reject anything that isn't a single safe token so a
    // token creator can't broaden the SUBSCRIBE grant via wildcards/extra tokens
    // (`*`, `>`, `.`, whitespace). Defended again in `mint_worker_jwt`.
    if let Some(group) = req.group.as_deref() {
        if !is_safe_group(group) {
            return Err(ApiError::bad_request(
                "group must be a single token of [A-Za-z0-9_-] (no '.', '*', '>', or whitespace)",
            ));
        }
        // A group is only meaningful when BACKED by a `capacity` resource at the
        // worker point in the trait-space (`competing_consumer` + `auto` — the
        // `worker` preset). Reject minting a token for an unbacked group so we
        // never enroll a worker whose group addresses no declared pool (the
        // silent dangling reference). The operator creates the group (a
        // `capacity` resource, `worker` preset) first.
        if !worker_group_exists(&state.db, workspace_id, group).await? {
            return Err(ApiError::bad_request(format!(
                "no worker group '{group}' exists in this workspace — create the group \
                 (a `capacity` resource with the `worker` preset) first, then mint a token for it"
            )));
        }
    }

    let reg_id = Uuid::new_v4();
    let minted = crate::models::runner::mint_token(WORKER_REG_TOKEN_PREFIX, reg_id);

    sqlx::query(
        "INSERT INTO worker_registration_tokens \
            (id, workspace_id, worker_group, token_hash, reusable, max_uses, expires_at, created_by) \
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
        Json(CreatedWorkerRegistrationToken {
            id: reg_id,
            token: minted.full_token,
            group: req.group,
            reusable,
            max_uses: req.max_uses,
            expires_at: req.expires_at,
        }),
    ))
}

/// `GET /api/v1/workers/registration-tokens` — paginated, workspace-scoped (live
/// tokens only). Never carries the hash.
#[utoipa::path(
    get,
    path = "/api/v1/workers/registration-tokens",
    params(ListWorkerRegTokensQuery),
    responses(
        (status = 200, description = "Paginated registration tokens", body = PaginatedResponse<WorkerRegistrationTokenSummary>),
    ),
    tag = "workers",
)]
pub async fn list_worker_registration_tokens(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListWorkerRegTokensQuery>,
) -> Result<Json<PaginatedResponse<WorkerRegistrationTokenSummary>>, ApiError> {
    let workspace_id = caller_workspace(&user)?;
    let offset = (params.page - 1) * params.per_page;

    let rows = sqlx::query_as::<_, WorkerRegistrationTokenRow>(
        "SELECT * FROM worker_registration_tokens \
         WHERE workspace_id = $1 AND revoked_at IS NULL \
         ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(workspace_id)
    .bind(params.per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM worker_registration_tokens \
         WHERE workspace_id = $1 AND revoked_at IS NULL",
    )
    .bind(workspace_id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(PaginatedResponse {
        items: rows
            .into_iter()
            .map(WorkerRegistrationTokenSummary::from)
            .collect(),
        total,
        page: params.page,
        per_page: params.per_page,
    }))
}

/// `DELETE /api/v1/workers/registration-tokens/{id}` — revoke a registration
/// token (soft delete; existing workers keep their credentials).
#[utoipa::path(
    delete,
    path = "/api/v1/workers/registration-tokens/{id}",
    params(("id" = Uuid, Path, description = "Registration token id")),
    responses(
        (status = 204, description = "Registration token revoked"),
        (status = 404, description = "Registration token not found", body = ErrorResponse),
    ),
    tag = "workers",
)]
pub async fn revoke_worker_registration_token(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let workspace_id = caller_workspace(&user)?;
    let updated = sqlx::query(
        "UPDATE worker_registration_tokens SET revoked_at = NOW() \
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
