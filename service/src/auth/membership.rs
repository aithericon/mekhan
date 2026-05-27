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
pub async fn member_role(db: &PgPool, user: &AuthUser, workspace_id: Uuid) -> Result<Role, MembershipError> {
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
pub async fn require_member(db: &PgPool, user: &AuthUser, workspace_id: Uuid) -> Result<Role, MembershipError> {
    require_role(db, user, workspace_id, Role::Viewer).await
}

/// Visibility values stored in `workflow_templates.visibility`. Kept here
/// (next to the permission rule) rather than in the template model so the
/// single source of truth for what `public` means lives with the gate.
const VISIBILITY_PUBLIC: &str = "public";

/// Resolve "can this user read this template?" — `true` when the template
/// is `visibility = 'public'` OR the user is a member of the template's
/// workspace. Errors when the template doesn't exist; callers translate
/// that to 404.
pub async fn can_read_template(db: &PgPool, user: &AuthUser, template_id: Uuid) -> Result<bool, MembershipError> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT workspace_id, visibility FROM workflow_templates WHERE id = $1",
    )
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
    row.map(|(w,)| w).ok_or(MembershipError::TemplateNotFound(template_id))
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
