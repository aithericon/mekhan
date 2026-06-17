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

/// Return the user's role in the given workspace, or `NotMember` if absent.
pub async fn member_role(
    db: &PgPool,
    user: &AuthUser,
    workspace_id: Uuid,
) -> Result<Role, MembershipError> {
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
}
