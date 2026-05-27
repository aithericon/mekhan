//! Multi-tenant test fixtures: seed workspaces, membership rows, and
//! workspace-scoped templates directly via SQL. Phase A2's tests need
//! more than the one default workspace the migrations seed; helpers
//! here let each test stand up its own isolated tenants.
#![allow(dead_code)]

use mekhan_service::auth::model::SUBJECT_UUID_NAMESPACE;
use sqlx::PgPool;
use uuid::Uuid;

/// Insert a workspace and return its id. Slug must be unique per process —
/// callers typically concatenate a `Uuid::new_v4().simple()` suffix.
pub async fn seed_workspace(db: &PgPool, slug: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workspaces (id, slug, display_name, is_system) \
              VALUES ($1, $2, $3, FALSE)",
    )
    .bind(id)
    .bind(slug)
    .bind(slug)
    .execute(db)
    .await
    .expect("seed workspace");
    id
}

/// Add a member by subject string (the mock authenticator's user id is
/// derived from this the same way the resolver derives it in production).
pub async fn seed_member(db: &PgPool, workspace_id: Uuid, subject: &str, role: &str) {
    let user_id = subject_uuid(subject);
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
              VALUES ($1, $2, $3) \
         ON CONFLICT (workspace_id, user_id) DO UPDATE SET role = EXCLUDED.role",
    )
    .bind(workspace_id)
    .bind(user_id)
    .bind(role)
    .execute(db)
    .await
    .expect("seed member");
}

/// Insert a template directly in the given workspace. Returns the template
/// id (which is also its base id since `version = 1`).
pub async fn seed_template_in_workspace(
    db: &PgPool,
    workspace_id: Uuid,
    name: &str,
    visibility: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    let author_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO workflow_templates \
            (id, name, description, version, is_latest, graph, author_id, workspace_id, visibility, published) \
         VALUES ($1, $2, '', 1, TRUE, '{}'::jsonb, $3, $4, $5, FALSE)",
    )
    .bind(id)
    .bind(name)
    .bind(author_id)
    .bind(workspace_id)
    .bind(visibility)
    .execute(db)
    .await
    .expect("seed template");
    id
}

/// `uuid_v5(SUBJECT_UUID_NAMESPACE, subject)` — the same derivation the
/// resolver and `AuthUser::subject_as_uuid` use, exposed for tests that
/// need to compare against the derived id.
pub fn subject_uuid(subject: &str) -> Uuid {
    Uuid::new_v5(&SUBJECT_UUID_NAMESPACE, subject.as_bytes())
}
