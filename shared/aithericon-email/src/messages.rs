//! Strongly-typed email payloads — one struct per email kind.
//!
//! Each implements [`TemplateMessage`]: it names a template under `templates/`,
//! a subject, and the variables that template renders. Branding/boilerplate is
//! injected by the [`Renderer`](crate::Renderer), so these carry only their own
//! fields. Add a new email kind by adding a struct + a template here.

use crate::port::TemplateMessage;

/// Invite a person to a workspace. Covers both a brand-new invitee and an
/// existing platform user (the `existing_user` flag flips the copy).
#[derive(Debug, Clone)]
pub struct WorkspaceInvite {
    /// Display name of the invitee, if known (else greet by email).
    pub recipient_name: Option<String>,
    /// Who sent the invite.
    pub inviter_name: String,
    /// Workspace they're invited to.
    pub workspace_name: String,
    /// Role being granted (owner | admin | editor | viewer).
    pub role: String,
    /// Fully-qualified accept link.
    pub accept_url: String,
    /// Human-readable expiry, e.g. "in 7 days" or an ISO date.
    pub expires: String,
    /// Whether the invitee already has a platform account.
    pub existing_user: bool,
}

impl TemplateMessage for WorkspaceInvite {
    fn template(&self) -> &'static str {
        "workspace_invite"
    }

    fn subject(&self) -> String {
        format!("You've been invited to {}", self.workspace_name)
    }

    fn context(&self) -> tera::Context {
        let mut ctx = tera::Context::new();
        ctx.insert("recipient_name", &self.recipient_name);
        ctx.insert("inviter_name", &self.inviter_name);
        ctx.insert("workspace_name", &self.workspace_name);
        ctx.insert("role", &self.role);
        ctx.insert("accept_url", &self.accept_url);
        ctx.insert("expires", &self.expires);
        ctx.insert("existing_user", &self.existing_user);
        ctx
    }
}

/// Tell a user a resource (folder / template / instance) was shared with them.
#[derive(Debug, Clone)]
pub struct ResourceShared {
    pub recipient_name: Option<String>,
    /// Who shared it.
    pub sharer_name: String,
    /// Object kind label, e.g. "template", "folder", "instance".
    pub object_kind: String,
    /// Display name of the shared object.
    pub object_name: String,
    /// Role granted on the object.
    pub role: String,
    /// Workspace the object lives in.
    pub workspace_name: String,
    /// Link straight to the object.
    pub url: String,
}

impl TemplateMessage for ResourceShared {
    fn template(&self) -> &'static str {
        "resource_shared"
    }

    fn subject(&self) -> String {
        format!(
            "{} shared a {} with you",
            self.sharer_name, self.object_kind
        )
    }

    fn context(&self) -> tera::Context {
        let mut ctx = tera::Context::new();
        ctx.insert("recipient_name", &self.recipient_name);
        ctx.insert("sharer_name", &self.sharer_name);
        ctx.insert("object_kind", &self.object_kind);
        ctx.insert("object_name", &self.object_name);
        ctx.insert("role", &self.role);
        ctx.insert("workspace_name", &self.workspace_name);
        ctx.insert("url", &self.url);
        ctx
    }
}

/// Tell a user they were added to a workspace (or their role changed).
#[derive(Debug, Clone)]
pub struct MemberAdded {
    pub recipient_name: Option<String>,
    /// Who performed the change.
    pub actor_name: String,
    pub workspace_name: String,
    /// The role they now hold.
    pub role: String,
    /// Link to the workspace.
    pub url: String,
    /// `true` ⇒ existing member's role changed; `false` ⇒ freshly added.
    pub role_changed: bool,
}

impl TemplateMessage for MemberAdded {
    fn template(&self) -> &'static str {
        "member_added"
    }

    fn subject(&self) -> String {
        if self.role_changed {
            format!("Your role in {} changed", self.workspace_name)
        } else {
            format!("You were added to {}", self.workspace_name)
        }
    }

    fn context(&self) -> tera::Context {
        let mut ctx = tera::Context::new();
        ctx.insert("recipient_name", &self.recipient_name);
        ctx.insert("actor_name", &self.actor_name);
        ctx.insert("workspace_name", &self.workspace_name);
        ctx.insert("role", &self.role);
        ctx.insert("url", &self.url);
        ctx.insert("role_changed", &self.role_changed);
        ctx
    }
}

/// Welcome a freshly provisioned user (first login / invite acceptance).
#[derive(Debug, Clone)]
pub struct Welcome {
    pub user_name: String,
    /// Workspace they just joined / landed in, if any.
    pub workspace_name: Option<String>,
    /// Where to log in / get started.
    pub login_url: String,
}

impl TemplateMessage for Welcome {
    fn template(&self) -> &'static str {
        "welcome"
    }

    fn subject(&self) -> String {
        "Welcome to Aithericon".to_string()
    }

    fn context(&self) -> tera::Context {
        let mut ctx = tera::Context::new();
        ctx.insert("user_name", &self.user_name);
        ctx.insert("workspace_name", &self.workspace_name);
        ctx.insert("login_url", &self.login_url);
        ctx
    }
}
