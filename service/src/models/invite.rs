//! Invite DTOs (Phase 4). The raw token NEVER appears on any wire type — it
//! only ever exists in the accept link delivered by email and the SHA-256 hash
//! stored in `pending_invites.token_hash`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Admin creates an invite. Optionally pre-seeds object grants applied on accept.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateInviteRequest {
    /// Invitee email (normalized lower-case server-side).
    pub email: String,
    /// Workspace role granted on accept. One of `owner|admin|editor|viewer`.
    pub role: String,
    /// Object grants applied (via `apply_grant`) on accept. Each object must be
    /// in this workspace and the caller's effective role on it must be ≥ the
    /// granted role (workspace Admin/Owner bypass).
    #[serde(default)]
    pub object_grants: Option<Vec<InviteObjectGrantSpec>>,
}

/// One pre-seeded object grant on an invite.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct InviteObjectGrantSpec {
    /// `folder` | `template` | `instance`.
    pub object_type: String,
    pub object_id: Uuid,
    /// `owner|admin|editor|viewer`.
    pub role: String,
}

/// Admin-facing invite row (never carries the token).
#[derive(Debug, Serialize, ToSchema)]
pub struct InviteSummary {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub email: String,
    pub role: String,
    /// `pending|accepted|revoked|expired`.
    pub status: String,
    pub invited_by: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invited_by_display_name: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Public invite preview shown on the accept page (minimal, non-enumerable).
#[derive(Debug, Serialize, ToSchema)]
pub struct InvitePreview {
    pub workspace_display_name: String,
    pub email: String,
    pub role: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
}

/// Result of accepting an invite.
#[derive(Debug, Serialize, ToSchema)]
pub struct AcceptInviteResponse {
    pub workspace_id: Uuid,
    /// `true` ⇒ the invitee now has a real IdP identity and the SPA must send
    /// them through `/api/auth/login` to obtain a session (mekhan does not mint
    /// it). `false` under `dev_noop` (every request is already the dev user).
    pub requires_login: bool,
}
