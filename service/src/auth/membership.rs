//! Single permission edge: workspace membership.
//!
//! Every gated handler ultimately answers two questions:
//!   - "Is this user a member of this workspace, and what role do they have?"
//!   - "Can this user read this template?" — which decomposes to "the
//!     template's workspace is the user's workspace, OR the template is
//!     public."
//!
//! Centralising the SQL here keeps the rule auditable in one place and
//! prevents the per-handler drift the resource_acl read-bypass memory
//! warned about. The helpers are thin — handlers call them inline, no
//! middleware indirection.

use sqlx::PgPool;
use uuid::Uuid;

use super::model::AuthUser;
use crate::models::error::ApiError;

/// Roles ordered by privilege. The check `role_at_least(actual, required)`
/// passes when `actual` rank ≥ `required` rank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer = 0,
    Editor = 1,
    Admin = 2,
    Owner = 3,
}

impl Role {
    pub fn from_db(s: &str) -> Option<Self> {
        match s {
            "viewer" => Some(Role::Viewer),
            "editor" => Some(Role::Editor),
            "admin" => Some(Role::Admin),
            "owner" => Some(Role::Owner),
            _ => None,
        }
    }

    /// Lowercase wire label (`owner|admin|editor|viewer`) — the inverse of
    /// [`Role::from_db`]. Used for the `my_effective_role` DTO annotation the
    /// SPA gates edit affordances on.
    pub fn as_label(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Editor => "editor",
            Role::Viewer => "viewer",
        }
    }
}

/// Reasons a permission check can fail. Distinct from `AuthError` because
/// these are authorization (the user is authenticated), not authentication.
#[derive(Debug, thiserror::Error)]
pub enum MembershipError {
    #[error("not a member of workspace {0}")]
    NotMember(Uuid),
    #[error("insufficient role: have {have:?}, need {need:?}")]
    InsufficientRole { have: Role, need: Role },
    #[error("template not found: {0}")]
    TemplateNotFound(Uuid),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// Resolve a MACHINE principal's role for `workspace_id` WITHOUT a DB hit.
///
/// A service-account token carries its workspace + role on the principal itself
/// — its authoritative source is the `service_accounts` row, NOT
/// `workspace_members` (a machine deliberately has no member row). So for the
/// SA's OWN fixed workspace we resolve the carried role; this is what makes
/// every data-plane gate (single-object reads AND mutations, via `member_role`
/// → `require_role` / `require_object_role`) consistent with the list endpoints,
/// which already scope by the carried `workspace_id`. Before this, a SA could
/// LIST its workspace's objects but 403'd opening or mutating any of them.
///
/// Returns:
///   - `Some(Ok(role))` — machine acting in its own workspace; the carried role.
///   - `Some(Err(NotMember))` — machine asking about a DIFFERENT workspace, or a
///     control-plane token (runner/worker) that carries no role at all.
///   - `None` — not a machine principal; the caller falls back to the
///     `workspace_members` table (the human path).
///
/// Note this resolves only the DATA-PLANE role. Identity/governance endpoints
/// (member/invite/role/ownership/lifecycle/credential-mint) refuse machine
/// principals outright via [`deny_machine_principal`], regardless of this role.
fn machine_member_role(
    user: &AuthUser,
    workspace_id: Uuid,
) -> Option<Result<Role, MembershipError>> {
    if !is_machine_principal(user) {
        return None;
    }
    if user.workspace_id == Some(workspace_id) {
        if let Some(role) = user.workspace_role.as_deref().and_then(Role::from_db) {
            return Some(Ok(role));
        }
    }
    Some(Err(MembershipError::NotMember(workspace_id)))
}

/// Return the user's role in the given workspace, or `NotMember` if absent.
pub async fn member_role(
    db: &PgPool,
    user: &AuthUser,
    workspace_id: Uuid,
) -> Result<Role, MembershipError> {
    // Machine principals (service accounts) resolve their role from the
    // principal itself, never from `workspace_members`. Runner/worker
    // control-plane tokens carry no role and fall out as `NotMember`.
    if let Some(resolved) = machine_member_role(user, workspace_id) {
        return resolved;
    }

    let user_id = user.subject_as_uuid();
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;

    match row.and_then(|(r,)| Role::from_db(&r)) {
        Some(r) => Ok(r),
        None => Err(MembershipError::NotMember(workspace_id)),
    }
}

/// Hard-gate a workspace operation: must be a member with at least `need`.
pub async fn require_role(
    db: &PgPool,
    user: &AuthUser,
    workspace_id: Uuid,
    need: Role,
) -> Result<Role, MembershipError> {
    let have = member_role(db, user, workspace_id).await?;
    if have < need {
        return Err(MembershipError::InsufficientRole { have, need });
    }
    Ok(have)
}

/// Convenience: `require_role(..., Role::Viewer)` — the basic read-membership
/// check used by GET / list endpoints scoped to a workspace.
pub async fn require_member(
    db: &PgPool,
    user: &AuthUser,
    workspace_id: Uuid,
) -> Result<Role, MembershipError> {
    require_role(db, user, workspace_id, Role::Viewer).await
}

/// Hard-gate a workspace ADMIN operation: must be a member with `Admin` or
/// `Owner`. Thin alias over [`require_role`] so admin-only management endpoints
/// (service accounts, invites) share one auditable gate; an insufficient role
/// surfaces as `InsufficientRole` → 403 via [`map_to_api_error`].
pub async fn require_workspace_admin(
    db: &PgPool,
    user: &AuthUser,
    workspace_id: Uuid,
) -> Result<Role, MembershipError> {
    require_role(db, user, workspace_id, Role::Admin).await
}

/// Whether a principal is a NON-human MACHINE credential — identified purely by
/// its `subject` prefix: `runner:` (runner token), `worker:` (worker token), or
/// `service-account:` (SA token). A human (cookie session or `uat_` PAT) carries
/// an OIDC `sub` (or `user:{uuid}`), which never matches.
///
/// Used as an explicit, intentional privilege-escalation guard at SA-management
/// call sites: only HUMAN admins may create/rotate service accounts, so a
/// service account can never mint more service accounts (lateral movement). This
/// is defense-in-depth — machine principals also have no `workspace_members` row,
/// so [`require_role`] already 403s them — but the explicit check states the
/// guarantee at the call site (the `auth_tokens.rs` `CookieAuthUser` precedent).
pub fn is_machine_principal(user: &AuthUser) -> bool {
    user.subject.starts_with("runner:")
        || user.subject.starts_with("worker:")
        || user.subject.starts_with("service-account:")
}

/// Refuse a non-human MACHINE principal from an IDENTITY / GOVERNANCE operation:
/// workspace member & invite management, role changes, ownership transfer,
/// workspace lifecycle (create/delete), and credential minting. A service
/// account is a DATA-PLANE principal — it carries a workspace role for OBJECTS
/// (templates/instances/resources/folders/assets/grants/…) but may never
/// administer the workspace's humans, its lifecycle, or credential families,
/// REGARDLESS of that role. This generalises the explicit guard that already
/// fronts SA-management (`service_accounts.rs::gate_human_admin`).
///
/// Call it FIRST in a governance handler (before the `require_role` admin gate)
/// so a machine principal gets a uniform 403 independent of its stored role —
/// otherwise an `admin`-role SA would now pass `require_role(.., Admin)` on
/// these endpoints (the very gates the carried-role fix above newly satisfies).
pub fn deny_machine_principal(user: &AuthUser) -> Result<(), ApiError> {
    if is_machine_principal(user) {
        return Err(ApiError::forbidden(
            "machine principals cannot perform workspace governance operations",
        ));
    }
    Ok(())
}

/// Read-gate a workspace that may be **world-readable**: pass with the caller's
/// real role when they're a member, OR with `Viewer` when the workspace is a
/// `is_system` one (e.g. the curated `demos` workspace). System workspaces are
/// browse-only destinations — any authenticated user may read them without a
/// membership row, which is what lets a user "visit" demos and fork from it
/// without the old cross-workspace public overlay polluting their own lists.
/// Mutating paths keep using `require_role(..., Editor)`, so this grants reads
/// only.
pub async fn require_workspace_read(
    db: &PgPool,
    user: &AuthUser,
    workspace_id: Uuid,
) -> Result<Role, MembershipError> {
    match member_role(db, user, workspace_id).await {
        Ok(r) => Ok(r),
        Err(MembershipError::NotMember(_)) => {
            let row: Option<(bool,)> = sqlx::query_as(
                "SELECT is_system FROM workspaces WHERE id = $1 AND archived_at IS NULL",
            )
            .bind(workspace_id)
            .fetch_optional(db)
            .await?;
            match row {
                Some((true,)) => Ok(Role::Viewer),
                _ => Err(MembershipError::NotMember(workspace_id)),
            }
        }
        Err(e) => Err(e),
    }
}

/// The workspace a fork should land in. A fork must go somewhere the caller can
/// write — but the caller may be *browsing* a read-only system workspace (demos)
/// when they fork, so the active workspace isn't always a valid target. Resolves
/// in order: an explicit `requested` target (must be Editor+), else the active
/// workspace when the caller is Editor+ there, else their first non-system
/// workspace where they're Editor+. `None` ⇒ the caller has nowhere to fork into
/// (callers map that to 400).
pub async fn resolve_fork_target(
    db: &PgPool,
    user: &AuthUser,
    requested: Option<Uuid>,
    active: Option<Uuid>,
) -> Result<Option<Uuid>, MembershipError> {
    if let Some(ws) = requested {
        return match require_role(db, user, ws, Role::Editor).await {
            Ok(_) => Ok(Some(ws)),
            Err(MembershipError::InsufficientRole { .. } | MembershipError::NotMember(_)) => {
                Ok(None)
            }
            Err(e) => Err(e),
        };
    }
    if let Some(ws) = active {
        if matches!(member_role(db, user, ws).await, Ok(r) if r >= Role::Editor) {
            return Ok(Some(ws));
        }
    }
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT w.id FROM workspaces w \
           JOIN workspace_members m ON m.workspace_id = w.id \
          WHERE m.user_id = $1 AND w.archived_at IS NULL AND w.is_system = FALSE \
            AND m.role IN ('editor', 'admin', 'owner') \
          ORDER BY w.created_at ASC LIMIT 1",
    )
    .bind(user.subject_as_uuid())
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(id,)| id))
}

/// Visibility values stored in `workflow_templates.visibility`. Kept here
/// (next to the permission rule) rather than in the template model so the
/// single source of truth for what `public` means lives with the gate.
const VISIBILITY_PUBLIC: &str = "public";

/// Resolve "can this user read this template?" — `true` when the template
/// is `visibility = 'public'` OR the user is a member of the template's
/// workspace. Errors when the template doesn't exist; callers translate
/// that to 404.
pub async fn can_read_template(
    db: &PgPool,
    user: &AuthUser,
    template_id: Uuid,
) -> Result<bool, MembershipError> {
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT workspace_id, visibility FROM workflow_templates WHERE id = $1")
            .bind(template_id)
            .fetch_optional(db)
            .await?;

    let (workspace_id, visibility) = row.ok_or(MembershipError::TemplateNotFound(template_id))?;

    if visibility == VISIBILITY_PUBLIC {
        return Ok(true);
    }

    match member_role(db, user, workspace_id).await {
        Ok(_) => Ok(true),
        Err(MembershipError::NotMember(_)) => Ok(false),
        Err(other) => Err(other),
    }
}

/// Looks up the workspace owning a template. Used by handlers that need to
/// authorize a mutating operation: read the template's workspace_id, then
/// `require_role(...)`.
pub async fn template_workspace(db: &PgPool, template_id: Uuid) -> Result<Uuid, MembershipError> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT workspace_id FROM workflow_templates WHERE id = $1")
            .bind(template_id)
            .fetch_optional(db)
            .await?;
    row.map(|(w,)| w)
        .ok_or(MembershipError::TemplateNotFound(template_id))
}

/// Fused lookup for the petri proxy hot path: given an engine `net_id`,
/// return the owning workspace + visibility of the template the instance
/// was deployed from. Saves one roundtrip versus
/// `template_id_for_instance(...)` + `template_workspace(...)`.
pub async fn instance_workspace(
    db: &PgPool,
    net_id: &str,
) -> Result<(Uuid, String), MembershipError> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT t.workspace_id, t.visibility \
           FROM workflow_instances i \
           JOIN workflow_templates t ON t.id = i.template_id \
          WHERE i.net_id = $1",
    )
    .bind(net_id)
    .fetch_optional(db)
    .await?;

    // No row -> treat as a missing template from the caller's perspective.
    // Carries the nil uuid because the net_id is the lookup key, not a
    // template uuid; callers translate the variant to 404 without inspecting
    // the inner id.
    row.ok_or(MembershipError::TemplateNotFound(Uuid::nil()))
}

/// Like [`instance_workspace`] but also returns the instance's own id, so the
/// caller can build an `ObjectRef::instance` for the object-ACL resolver. `None`
/// when the `net_id` isn't a mekhan-managed instance (an infra net) — callers
/// translate that to the safe-method-allow branch.
pub async fn instance_ref_by_net_id(
    db: &PgPool,
    net_id: &str,
) -> Result<Option<(Uuid, Uuid, String)>, MembershipError> {
    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT i.id, t.workspace_id, t.visibility \
           FROM workflow_instances i \
           JOIN workflow_templates t ON t.id = i.template_id \
          WHERE i.net_id = $1",
    )
    .bind(net_id)
    .fetch_optional(db)
    .await?;
    Ok(row)
}

/// Translate a `MembershipError` to the standard `ApiError` shape used by
/// handler gates. Lifted out of the per-handler match blocks so new gates
/// don't re-paste the 8-line pattern.
pub fn map_to_api_error(err: MembershipError) -> ApiError {
    match err {
        MembershipError::NotMember(_) => ApiError::forbidden("not a member of this workspace"),
        MembershipError::InsufficientRole { need, .. } => {
            ApiError::forbidden(format!("{:?} role required", need).to_lowercase())
        }
        MembershipError::TemplateNotFound(_) => ApiError::not_found("template not found"),
        MembershipError::Db(e) => ApiError::internal(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_ordering() {
        assert!(Role::Owner > Role::Admin);
        assert!(Role::Admin > Role::Editor);
        assert!(Role::Editor > Role::Viewer);
        assert!(Role::Viewer == Role::Viewer);
    }

    #[test]
    fn role_from_db_known_values() {
        assert_eq!(Role::from_db("owner"), Some(Role::Owner));
        assert_eq!(Role::from_db("admin"), Some(Role::Admin));
        assert_eq!(Role::from_db("editor"), Some(Role::Editor));
        assert_eq!(Role::from_db("viewer"), Some(Role::Viewer));
        assert_eq!(Role::from_db("nonsense"), None);
    }

    #[test]
    fn role_at_least_semantics() {
        // editor satisfies a viewer-or-above requirement; viewer doesn't
        // satisfy an editor-or-above requirement.
        assert!(Role::Editor >= Role::Viewer);
        assert!(Role::Viewer < Role::Editor);
        assert!(Role::Owner >= Role::Owner);
    }

    fn user_with_subject(subject: &str) -> AuthUser {
        AuthUser {
            subject: subject.to_string(),
            user_id: AuthUser::legacy_subject_uuid(subject),
            email: None,
            display_name: None,
            roles: Vec::new(),
            is_platform_admin: false,
            workspace_id: None,
            workspace_role: None,
            avatar_url: None,
        }
    }

    #[test]
    fn machine_principal_detects_every_machine_prefix() {
        assert!(is_machine_principal(&user_with_subject(
            "runner:11111111-1111-1111-1111-111111111111"
        )));
        assert!(is_machine_principal(&user_with_subject(
            "worker:22222222-2222-2222-2222-222222222222"
        )));
        assert!(is_machine_principal(&user_with_subject(
            "service-account:33333333-3333-3333-3333-333333333333"
        )));
    }

    #[test]
    fn machine_principal_false_for_humans() {
        // A real OIDC sub and the `user:{uuid}` fallback are both human.
        assert!(!is_machine_principal(&user_with_subject("zitadel|abc-123")));
        assert!(!is_machine_principal(&user_with_subject(
            "user:44444444-4444-4444-4444-444444444444"
        )));
    }

    fn sa_user(workspace: Uuid, role: Option<&str>) -> AuthUser {
        let mut u = user_with_subject("service-account:55555555-5555-5555-5555-555555555555");
        u.workspace_id = Some(workspace);
        u.workspace_role = role.map(str::to_string);
        u
    }

    #[test]
    fn machine_member_role_honors_carried_role_in_own_workspace() {
        // The core fix: an SA resolves its fixed role for its own workspace
        // without a `workspace_members` row — so detail reads + mutations gate
        // consistently with the list endpoints.
        let ws = Uuid::from_u128(0xabc);
        for (label, want) in [
            ("viewer", Role::Viewer),
            ("editor", Role::Editor),
            ("admin", Role::Admin),
        ] {
            let u = sa_user(ws, Some(label));
            assert_eq!(machine_member_role(&u, ws).unwrap().unwrap(), want);
        }
    }

    #[test]
    fn machine_member_role_not_member_for_other_workspace() {
        // An SA is pinned to ONE workspace; it is never a member elsewhere.
        let ws = Uuid::from_u128(0xabc);
        let other = Uuid::from_u128(0xdef);
        let u = sa_user(ws, Some("admin"));
        assert!(matches!(
            machine_member_role(&u, other),
            Some(Err(MembershipError::NotMember(_)))
        ));
    }

    #[test]
    fn machine_member_role_not_member_when_no_carried_role() {
        // Runner/worker control-plane tokens carry a workspace but no role —
        // they must NOT be silently elevated, so they resolve to NotMember.
        let ws = Uuid::from_u128(0xabc);
        let mut u = user_with_subject("runner:11111111-1111-1111-1111-111111111111");
        u.workspace_id = Some(ws);
        u.workspace_role = None;
        assert!(matches!(
            machine_member_role(&u, ws),
            Some(Err(MembershipError::NotMember(_)))
        ));
    }

    #[test]
    fn machine_member_role_none_for_humans() {
        // Humans always fall through to the `workspace_members` table, even
        // when a carried role is present (it's only an advisory cache for them).
        let ws = Uuid::from_u128(0xabc);
        let mut u = user_with_subject("zitadel|abc-123");
        u.workspace_id = Some(ws);
        u.workspace_role = Some("admin".into());
        assert!(machine_member_role(&u, ws).is_none());
    }

    #[test]
    fn deny_machine_principal_blocks_machines_allows_humans() {
        assert!(deny_machine_principal(&user_with_subject(
            "service-account:33333333-3333-3333-3333-333333333333"
        ))
        .is_err());
        assert!(deny_machine_principal(&user_with_subject(
            "runner:11111111-1111-1111-1111-111111111111"
        ))
        .is_err());
        assert!(deny_machine_principal(&user_with_subject(
            "worker:22222222-2222-2222-2222-222222222222"
        ))
        .is_err());
        assert!(deny_machine_principal(&user_with_subject("zitadel|abc-123")).is_ok());
        assert!(deny_machine_principal(&user_with_subject(
            "user:44444444-4444-4444-4444-444444444444"
        ))
        .is_ok());
    }
}
