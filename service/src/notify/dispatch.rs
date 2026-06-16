//! Trigger helpers that turn a control-plane event (grant applied, member added,
//! user provisioned) into a typed email and hand it to the [`Mailer`] port.
//!
//! Every function here is **best-effort and non-fatal**: it resolves display
//! names / links with small queries, sends, and only logs on failure. The
//! caller's primary action (the grant, the membership) has already committed —
//! a missing email must never fail the request. Default delivery is the offline
//! log mailer.

use uuid::Uuid;

use crate::auth::ObjectKind;
use crate::notify::email::{MemberAdded, Recipient, ResourceShared, Welcome};
use crate::AppState;

fn base_url(state: &AppState) -> String {
    state
        .config
        .email
        .public_base_url
        .trim_end_matches('/')
        .to_string()
}

async fn workspace_name(state: &AppState, workspace_id: Uuid) -> String {
    sqlx::query_scalar::<_, String>("SELECT display_name FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "workspace".to_string())
}

/// Human label for an object kind, used in subject/body copy.
fn kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Folder => "folder",
        ObjectKind::Template => "template",
        ObjectKind::Instance => "instance",
        ObjectKind::Resource => "resource",
        ObjectKind::Asset => "asset",
    }
}

/// Best-effort display name for a shared object; falls back to the kind label.
async fn object_name(state: &AppState, kind: ObjectKind, id: Uuid) -> String {
    let name: Option<String> = match kind {
        ObjectKind::Folder => sqlx::query_scalar("SELECT display_name FROM folders WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten(),
        ObjectKind::Template => {
            sqlx::query_scalar("SELECT name FROM workflow_templates WHERE id = $1")
                .bind(id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten()
        }
        ObjectKind::Instance => sqlx::query_scalar(
            "SELECT t.name FROM workflow_instances i \
               JOIN workflow_templates t ON t.id = i.template_id WHERE i.id = $1",
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten(),
        ObjectKind::Resource | ObjectKind::Asset => None,
    };
    name.unwrap_or_else(|| kind_label(kind).to_string())
}

/// Deep link to an object in the SPA. Routes mirror the frontend; a wrong link
/// still leaves the email informative.
fn object_link(state: &AppState, kind: ObjectKind, id: Uuid) -> String {
    let base = base_url(state);
    match kind {
        ObjectKind::Folder => format!("{base}/folders/{id}"),
        ObjectKind::Template => format!("{base}/templates/{id}"),
        ObjectKind::Instance => format!("{base}/instances/{id}"),
        ObjectKind::Resource => format!("{base}/resources/{id}"),
        ObjectKind::Asset => base,
    }
}

/// Notify a grantee that an object was shared with them. `to_email` may be
/// absent (no profile yet) — then we skip silently.
#[allow(clippy::too_many_arguments)]
pub async fn resource_shared(
    state: &AppState,
    to_email: Option<&str>,
    to_name: Option<&str>,
    sharer_name: &str,
    kind: ObjectKind,
    object_id: Uuid,
    workspace_id: Uuid,
    role: &str,
) {
    let Some(email) = to_email.filter(|e| !e.trim().is_empty()) else {
        return;
    };
    let msg = ResourceShared {
        recipient_name: to_name.map(str::to_string),
        sharer_name: sharer_name.to_string(),
        object_kind: kind_label(kind).to_string(),
        object_name: object_name(state, kind, object_id).await,
        role: role.to_string(),
        workspace_name: workspace_name(state, workspace_id).await,
        url: object_link(state, kind, object_id),
    };
    if let Err(e) = state.email.send(&Recipient::new(email), &msg).await {
        tracing::warn!(%email, "resource-shared email send failed: {e}");
    }
}

/// Notify a user they were added to a workspace (or their role changed).
#[allow(clippy::too_many_arguments)]
pub async fn member_added(
    state: &AppState,
    to_email: Option<&str>,
    to_name: Option<&str>,
    actor_name: &str,
    workspace_id: Uuid,
    role: &str,
    role_changed: bool,
) {
    let Some(email) = to_email.filter(|e| !e.trim().is_empty()) else {
        return;
    };
    let msg = MemberAdded {
        recipient_name: to_name.map(str::to_string),
        actor_name: actor_name.to_string(),
        workspace_name: workspace_name(state, workspace_id).await,
        role: role.to_string(),
        url: base_url(state),
        role_changed,
    };
    if let Err(e) = state.email.send(&Recipient::new(email), &msg).await {
        tracing::warn!(%email, "member-added email send failed: {e}");
    }
}

/// Welcome a freshly provisioned user.
pub async fn welcome(
    state: &AppState,
    to_email: &str,
    user_name: &str,
    workspace_id: Option<Uuid>,
) {
    if to_email.trim().is_empty() {
        return;
    }
    let workspace_name = match workspace_id {
        Some(id) => Some(workspace_name(state, id).await),
        None => None,
    };
    let msg = Welcome {
        user_name: user_name.to_string(),
        workspace_name,
        login_url: base_url(state),
    };
    if let Err(e) = state.email.send(&Recipient::new(to_email), &msg).await {
        tracing::warn!(%to_email, "welcome email send failed: {e}");
    }
}
