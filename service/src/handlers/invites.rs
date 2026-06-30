//! Invite lifecycle (Phase 4). Admin creates/lists/resends/revokes invites;
//! the invitee accepts — while logged in — via a token link that applies the
//! workspace membership + pre-seeded object grants in one transaction.
//!
//! Identity is fully in-app: there is no IdP provisioner. The accept endpoint
//! is AUTHED — the logged-in session IS the joining identity. The handler
//! requires the session's email to match the invite email (case-insensitive),
//! then writes `workspace_members` / object grants keyed by the session's
//! resolved `user_id` (`users.id`).
//!
//! Security: the raw token is a 32-byte CSPRNG value (base64url) sent only in
//! the accept link; only its SHA-256 hash is stored. `preview` (public) and
//! `accept` return a single generic 404 for unknown/expired/revoked/accepted
//! (one code path, no enumeration). The accept re-checks `status='pending'`
//! under `SELECT … FOR UPDATE` for single-use atomicity.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::Engine;
use chrono::{Duration, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::auth::{
    apply_grant, deny_machine_principal, effective_object_role, grant_context, map_to_api_error,
    require_role, AuthUser,
    ObjectKind, ObjectRef, Role,
};
use crate::models::error::{ApiError, ErrorResponse};
use crate::models::invite::{
    AcceptInviteResponse, CreateInviteRequest, InvitePreview, InviteSummary,
};
use crate::AppState;

// ── token helpers ────────────────────────────────────────────────────────────

/// 32 bytes of CSPRNG → base64url (no pad, ~43 chars). Infeasible to brute force.
fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// SHA-256 of the raw token (raw bytes for the BYTEA column). The raw token is
/// never stored.
fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn parse_object_kind(s: &str) -> Option<ObjectKind> {
    match s {
        "folder" => Some(ObjectKind::Folder),
        "template" => Some(ObjectKind::Template),
        "instance" => Some(ObjectKind::Instance),
        _ => None,
    }
}

fn accept_url(state: &AppState, token: &str) -> String {
    format!(
        "{}/invite/accept?token={}",
        state.config.email.public_base_url.trim_end_matches('/'),
        token
    )
}

/// Render + send the workspace-invite email. Best-effort: a delivery failure is
/// logged, never propagated — the invite row already exists and an Admin can
/// resend. Delivery defaults to the offline log mailer.
async fn send_invite_email(
    state: &AppState,
    to_email: &str,
    token: &str,
    workspace_name: &str,
    inviter_name: &str,
    role: &str,
    expires_at: chrono::DateTime<Utc>,
) {
    let msg = crate::notify::email::WorkspaceInvite {
        recipient_name: None,
        inviter_name: inviter_name.to_string(),
        workspace_name: workspace_name.to_string(),
        role: role.to_string(),
        accept_url: accept_url(state, token),
        expires: format!("on {}", expires_at.format("%Y-%m-%d")),
        existing_user: false,
    };
    if let Err(e) = state
        .email
        .send(&crate::notify::email::Recipient::new(to_email), &msg)
        .await
    {
        tracing::warn!(%to_email, "invite email send failed: {e}");
    }
}

// ── admin endpoints ──────────────────────────────────────────────────────────

/// POST /api/v1/workspaces/{id}/invites — Admin-gated. Creates (or rotates a
/// duplicate-active) invite and sends the accept link. 201 on create, 200 on
/// rotate. Never returns the raw token.
#[utoipa::path(
    post,
    path = "/api/v1/workspaces/{id}/invites",
    params(("id" = Uuid, Path, description = "Workspace id")),
    request_body = CreateInviteRequest,
    responses(
        (status = 201, description = "Invite created", body = InviteSummary),
        (status = 200, description = "Existing active invite rotated + resent", body = InviteSummary),
        (status = 400, description = "Invalid role / cross-workspace grant", body = ErrorResponse),
        (status = 403, description = "Admin role required / grant escalation", body = ErrorResponse),
    ),
    tag = "invites",
)]
pub async fn create_invite(
    State(state): State<AppState>,
    user: AuthUser,
    Path(workspace_id): Path<Uuid>,
    Json(req): Json<CreateInviteRequest>,
) -> Result<(StatusCode, Json<InviteSummary>), ApiError> {
    deny_machine_principal(&user)?;
    require_role(&state.db, &user, workspace_id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    if Role::from_db(&req.role).is_none() {
        return Err(ApiError::bad_request(format!(
            "unknown role '{}', expected one of owner|admin|editor|viewer",
            req.role
        )));
    }
    let email = req.email.trim().to_ascii_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(ApiError::bad_request("email is empty or missing '@'"));
    }

    // Validate each pre-seeded object grant: parseable kind, valid role, object
    // lives in THIS workspace, and the caller's effective role on it ≥ the
    // granted role (workspace Admin/Owner bypass already widened it).
    let specs = req.object_grants.clone().unwrap_or_default();
    for g in &specs {
        let kind = parse_object_kind(&g.object_type).ok_or_else(|| {
            ApiError::bad_request(format!("unknown object_type '{}'", g.object_type))
        })?;
        let grant_role = Role::from_db(&g.role)
            .ok_or_else(|| ApiError::bad_request(format!("unknown grant role '{}'", g.role)))?;
        let ctx = grant_context(
            &state.db,
            ObjectRef {
                kind,
                id: g.object_id,
            },
        )
        .await
        .map_err(map_to_api_error)?
        .ok_or_else(|| ApiError::bad_request("grant object not found"))?;
        if ctx.workspace_id != workspace_id {
            return Err(ApiError::bad_request(
                "grant object is not in this workspace",
            ));
        }
        match effective_object_role(
            &state.db,
            &user,
            ObjectRef {
                kind,
                id: g.object_id,
            },
        )
        .await
        .map_err(map_to_api_error)?
        {
            Some(caller) if caller >= grant_role => {}
            _ => {
                return Err(ApiError::forbidden(
                    "cannot grant a role higher than your own on a target object",
                ))
            }
        }
    }

    let token = generate_token();
    let token_hash = hash_token(&token);
    let expires_at = Utc::now() + Duration::seconds(state.config.email.invite_ttl_secs);
    let invited_by = user.subject_as_uuid();

    // Rotate an existing active invite for this email, else insert a new one.
    let mut tx = state.db.begin().await?;
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM pending_invites \
          WHERE workspace_id = $1 AND lower(email) = $2 AND status = 'pending'",
    )
    .bind(workspace_id)
    .bind(&email)
    .fetch_optional(&mut *tx)
    .await?;

    let (invite_id, created_status) = match existing {
        Some((id,)) => {
            sqlx::query(
                "UPDATE pending_invites SET role = $2, token_hash = $3, invited_by = $4, \
                        created_at = now(), expires_at = $5 WHERE id = $1",
            )
            .bind(id)
            .bind(&req.role)
            .bind(&token_hash)
            .bind(invited_by)
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;
            // Replace its pre-seeded grants with the new set.
            sqlx::query("DELETE FROM invite_object_grants WHERE invite_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            (id, StatusCode::OK)
        }
        None => {
            let row: (Uuid,) = sqlx::query_as(
                "INSERT INTO pending_invites (workspace_id, email, role, token_hash, invited_by, expires_at) \
                      VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
            )
            .bind(workspace_id)
            .bind(&email)
            .bind(&req.role)
            .bind(&token_hash)
            .bind(invited_by)
            .bind(expires_at)
            .fetch_one(&mut *tx)
            .await?;
            (row.0, StatusCode::CREATED)
        }
    };

    for g in &specs {
        sqlx::query(
            "INSERT INTO invite_object_grants (invite_id, object_type, object_id, role) \
                  VALUES ($1, $2, $3, $4)",
        )
        .bind(invite_id)
        .bind(&g.object_type)
        .bind(g.object_id)
        .bind(&g.role)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    // Send the accept link (log-mode by default). Non-fatal: the invite exists;
    // an Admin can resend.
    let ws_name: String = sqlx::query_scalar("SELECT display_name FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "workspace".to_string());
    let inviter = user
        .display_name
        .clone()
        .unwrap_or_else(|| "an admin".into());
    send_invite_email(
        &state, &email, &token, &ws_name, &inviter, &req.role, expires_at,
    )
    .await;

    let summary = load_invite_summary(&state, invite_id)
        .await?
        .ok_or_else(|| ApiError::internal("invite vanished after create"))?;
    Ok((created_status, Json(summary)))
}

/// GET /api/v1/workspaces/{id}/invites — Admin-gated list.
#[utoipa::path(
    get,
    path = "/api/v1/workspaces/{id}/invites",
    params(("id" = Uuid, Path, description = "Workspace id")),
    responses(
        (status = 200, description = "Invites for the workspace", body = Vec<InviteSummary>),
        (status = 403, description = "Admin role required", body = ErrorResponse),
    ),
    tag = "invites",
)]
pub async fn list_invites(
    State(state): State<AppState>,
    user: AuthUser,
    Path(workspace_id): Path<Uuid>,
) -> Result<Json<Vec<InviteSummary>>, ApiError> {
    deny_machine_principal(&user)?;
    require_role(&state.db, &user, workspace_id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;
    let rows: Vec<InviteRow> = sqlx::query_as(
        "SELECT i.id, i.workspace_id, i.email, i.role, i.status, i.invited_by, \
                p.display_name AS invited_by_display_name, i.created_at, i.expires_at \
           FROM pending_invites i \
           LEFT JOIN users p ON p.id = i.invited_by \
          WHERE i.workspace_id = $1 \
          ORDER BY i.created_at DESC",
    )
    .bind(workspace_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("list invites: {e}")))?;
    Ok(Json(
        rows.into_iter().map(InviteRow::into_summary).collect(),
    ))
}

/// POST /api/v1/workspaces/{id}/invites/{invite_id}/resend — Admin-gated.
/// Rotates the token + expiry (old link dies) and resends.
#[utoipa::path(
    post,
    path = "/api/v1/workspaces/{id}/invites/{invite_id}/resend",
    params(
        ("id" = Uuid, Path, description = "Workspace id"),
        ("invite_id" = Uuid, Path, description = "Invite id"),
    ),
    responses(
        (status = 200, description = "Rotated + resent", body = InviteSummary),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 404, description = "No pending invite", body = ErrorResponse),
    ),
    tag = "invites",
)]
pub async fn resend_invite(
    State(state): State<AppState>,
    user: AuthUser,
    Path((workspace_id, invite_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<InviteSummary>, ApiError> {
    deny_machine_principal(&user)?;
    require_role(&state.db, &user, workspace_id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    let token = generate_token();
    let token_hash = hash_token(&token);
    let expires_at = Utc::now() + Duration::seconds(state.config.email.invite_ttl_secs);

    let updated: Option<(String, String)> = sqlx::query_as(
        "UPDATE pending_invites SET token_hash = $3, expires_at = $4, created_at = now() \
          WHERE id = $1 AND workspace_id = $2 AND status = 'pending' RETURNING email, role",
    )
    .bind(invite_id)
    .bind(workspace_id)
    .bind(&token_hash)
    .bind(expires_at)
    .fetch_optional(&state.db)
    .await?;
    let (email, role) = updated.ok_or_else(|| ApiError::not_found("no pending invite"))?;

    let ws_name: String = sqlx::query_scalar("SELECT display_name FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "workspace".to_string());
    let inviter = user
        .display_name
        .clone()
        .unwrap_or_else(|| "an admin".into());
    send_invite_email(
        &state, &email, &token, &ws_name, &inviter, &role, expires_at,
    )
    .await;

    let summary = load_invite_summary(&state, invite_id)
        .await?
        .ok_or_else(|| ApiError::not_found("no pending invite"))?;
    Ok(Json(summary))
}

/// DELETE /api/v1/workspaces/{id}/invites/{invite_id} — Admin-gated revoke.
/// Idempotent (already-revoked → 204); an accepted invite → 409.
#[utoipa::path(
    delete,
    path = "/api/v1/workspaces/{id}/invites/{invite_id}",
    params(
        ("id" = Uuid, Path, description = "Workspace id"),
        ("invite_id" = Uuid, Path, description = "Invite id"),
    ),
    responses(
        (status = 204, description = "Revoked"),
        (status = 403, description = "Admin role required", body = ErrorResponse),
        (status = 404, description = "Not found", body = ErrorResponse),
        (status = 409, description = "Already accepted", body = ErrorResponse),
    ),
    tag = "invites",
)]
pub async fn revoke_invite(
    State(state): State<AppState>,
    user: AuthUser,
    Path((workspace_id, invite_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, ApiError> {
    deny_machine_principal(&user)?;
    require_role(&state.db, &user, workspace_id, Role::Admin)
        .await
        .map_err(map_to_api_error)?;

    let row: Option<(String,)> =
        sqlx::query_as("SELECT status FROM pending_invites WHERE id = $1 AND workspace_id = $2")
            .bind(invite_id)
            .bind(workspace_id)
            .fetch_optional(&state.db)
            .await?;
    let (status,) = row.ok_or_else(|| ApiError::not_found("invite not found"))?;
    match status.as_str() {
        "accepted" => return Err(ApiError::conflict("invite already accepted")),
        "revoked" => return Ok(StatusCode::NO_CONTENT), // idempotent
        _ => {}
    }
    sqlx::query("UPDATE pending_invites SET status = 'revoked', revoked_at = now() WHERE id = $1")
        .bind(invite_id)
        .execute(&state.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ── public endpoints ─────────────────────────────────────────────────────────

/// GET /api/v1/invites/{token}/preview — PUBLIC. Generic 404 for any
/// unknown/expired/revoked/accepted token (single code path, no enumeration).
#[utoipa::path(
    get,
    path = "/api/v1/invites/{token}/preview",
    params(("token" = String, Path, description = "Opaque invite token")),
    responses(
        (status = 200, description = "Invite preview", body = InvitePreview),
        (status = 404, description = "No valid invite", body = ErrorResponse),
    ),
    tag = "invites",
)]
pub async fn preview_invite(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<InvitePreview>, ApiError> {
    let token_hash = hash_token(&token);
    let row: Option<(Uuid, String, String, chrono::DateTime<Utc>)> = sqlx::query_as(
        "SELECT workspace_id, email, role, expires_at FROM pending_invites \
          WHERE token_hash = $1 AND status = 'pending' AND expires_at > now()",
    )
    .bind(&token_hash)
    .fetch_optional(&state.db)
    .await?;
    let (workspace_id, email, role, expires_at) =
        row.ok_or_else(|| ApiError::not_found("invite is not valid"))?;
    let ws_name: String = sqlx::query_scalar("SELECT display_name FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "workspace".to_string());
    Ok(Json(InvitePreview {
        workspace_display_name: ws_name,
        email,
        role,
        status: "pending".to_string(),
        expires_at,
    }))
}

/// POST /api/v1/invites/{token}/accept — AUTHED. The logged-in session IS the
/// joining identity: the handler requires the session email to match the invite
/// email, then atomically applies membership + grants keyed by the session's
/// resolved `user_id`. Single-use via `SELECT … FOR UPDATE` re-checking
/// `status='pending'`.
#[utoipa::path(
    post,
    path = "/api/v1/invites/{token}/accept",
    params(("token" = String, Path, description = "Opaque invite token")),
    responses(
        (status = 200, description = "Accepted", body = AcceptInviteResponse),
        (status = 403, description = "Signed-in email does not match the invite", body = ErrorResponse),
        (status = 404, description = "No valid invite", body = ErrorResponse),
    ),
    tag = "invites",
)]
pub async fn accept_invite(
    State(state): State<AppState>,
    user: AuthUser,
    Path(token): Path<String>,
) -> Result<Json<AcceptInviteResponse>, ApiError> {
    // Only a human identity can join a workspace via an invite — a machine
    // principal has no email to match and must never be enrolled as a member.
    deny_machine_principal(&user)?;
    let token_hash = hash_token(&token);

    // Read the invite (unlocked) to get the target email. The tx below re-checks
    // status under FOR UPDATE for single-use atomicity.
    let invite: Option<(Uuid, Uuid, String, String, Uuid)> = sqlx::query_as(
        "SELECT id, workspace_id, email, role, invited_by FROM pending_invites \
          WHERE token_hash = $1 AND status = 'pending' AND expires_at > now()",
    )
    .bind(&token_hash)
    .fetch_optional(&state.db)
    .await?;
    let (invite_id, workspace_id, email, role_str, invited_by) =
        invite.ok_or_else(|| ApiError::not_found("invite is not valid"))?;

    // The logged-in user IS the joining identity. Require their session email to
    // match the invite email (case-insensitive); otherwise refuse so an invite
    // can't be redeemed onto a different account.
    let session_email = user.email.as_deref().unwrap_or_default().trim();
    if session_email.is_empty() || !session_email.eq_ignore_ascii_case(email.trim()) {
        let signed_in_as = if session_email.is_empty() {
            "an account with no email".to_string()
        } else {
            format!("'{session_email}'")
        };
        return Err(ApiError::forbidden(format!(
            "this invite was sent to '{email}'; you are signed in as {signed_in_as}"
        )));
    }

    let user_id = user.subject_as_uuid();

    let mut tx = state.db.begin().await?;
    // Single-use lock + re-check.
    let still_pending: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM pending_invites \
          WHERE id = $1 AND status = 'pending' AND expires_at > now() FOR UPDATE",
    )
    .bind(invite_id)
    .fetch_optional(&mut *tx)
    .await?;
    if still_pending.is_none() {
        return Err(ApiError::not_found("invite is not valid"));
    }

    // Upsert workspace membership at the invited role.
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) VALUES ($1, $2, $3) \
         ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = EXCLUDED.role",
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(&role_str)
    .execute(&mut *tx)
    .await?;

    // Apply each pre-seeded object grant.
    let grants: Vec<(String, Uuid, String)> = sqlx::query_as(
        "SELECT object_type, object_id, role FROM invite_object_grants WHERE invite_id = $1",
    )
    .bind(invite_id)
    .fetch_all(&mut *tx)
    .await?;
    for (otype, oid, grole) in grants {
        let (Some(kind), Some(role)) = (parse_object_kind(&otype), Role::from_db(&grole)) else {
            continue; // CHECK constraints make this unreachable; skip defensively.
        };
        apply_grant(&mut *tx, workspace_id, kind, oid, user_id, role, invited_by)
            .await
            .map_err(|e| ApiError::internal(format!("apply invite grant: {e}")))?;
    }

    sqlx::query(
        "UPDATE pending_invites SET status = 'accepted', accepted_at = now(), \
                accepted_user_id = $2 WHERE id = $1",
    )
    .bind(invite_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(AcceptInviteResponse { workspace_id }))
}

// ── row mapping ──────────────────────────────────────────────────────────────

#[derive(sqlx::FromRow)]
struct InviteRow {
    id: Uuid,
    workspace_id: Uuid,
    email: String,
    role: String,
    status: String,
    invited_by: Uuid,
    invited_by_display_name: Option<String>,
    created_at: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
}

impl InviteRow {
    fn into_summary(self) -> InviteSummary {
        InviteSummary {
            id: self.id,
            workspace_id: self.workspace_id,
            email: self.email,
            role: self.role,
            status: self.status,
            invited_by: self.invited_by,
            invited_by_display_name: self.invited_by_display_name,
            created_at: self.created_at,
            expires_at: self.expires_at,
        }
    }
}

async fn load_invite_summary(
    state: &AppState,
    invite_id: Uuid,
) -> Result<Option<InviteSummary>, ApiError> {
    let row: Option<InviteRow> = sqlx::query_as(
        "SELECT i.id, i.workspace_id, i.email, i.role, i.status, i.invited_by, \
                p.display_name AS invited_by_display_name, i.created_at, i.expires_at \
           FROM pending_invites i \
           LEFT JOIN users p ON p.id = i.invited_by \
          WHERE i.id = $1",
    )
    .bind(invite_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::internal(format!("load invite: {e}")))?;
    Ok(row.map(InviteRow::into_summary))
}
