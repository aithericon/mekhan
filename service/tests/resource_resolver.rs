//! Integration tests for `ResourceResolver` (Phase B.5).
//!
//! Talks to the shared test Postgres (`localhost:5599`) via the same
//! `create_test_db()` helper that the rest of the suite uses. Each test
//! gets a freshly-migrated DB so seeded resources / ACL / audit rows are
//! isolated from siblings.
//!
//! Skipping behavior: if `TEST_POSTGRES_URL` (or the default at port 5599)
//! is unreachable, `create_test_db()` panics. That mirrors how every other
//! DB-backed test in this crate behaves; CI runs `just dev::up` first.

mod common;

use std::collections::HashMap;

use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use aithericon_resources::ResourcePin;
use mekhan_service::handlers::resources::vault_path_for;
use mekhan_service::petri::resource_resolver::{
    AuditAction, AuditContext, ResolverError, ResourceResolver,
};

// ── Seeding helpers ───────────────────────────────────────────────────────

/// Insert a `resources` + `resource_versions` pair. Returns the new
/// resource's id. `public_config` is whatever JSON the test wants the
/// resolver to read back.
async fn seed_resource(
    db: &PgPool,
    workspace_id: Uuid,
    creator: Uuid,
    resource_type: &str,
    path: &str,
    public_config: serde_json::Value,
) -> Uuid {
    let resource_id = Uuid::new_v4();
    let vault_path = vault_path_for(workspace_id, resource_id, 1);

    // The resources_workspace_fk (migration 20240126) requires the workspace
    // row to exist before a resource can reference it. Seed it idempotently so
    // each fixture stays self-contained (callers pass an ad-hoc workspace_id).
    sqlx::query(
        "INSERT INTO workspaces (id, slug, display_name) \
            VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING",
    )
    .bind(workspace_id)
    .bind(format!("ws-{workspace_id}"))
    .bind("Test Workspace")
    .execute(db)
    .await
    .expect("seed workspace for resource FK");

    // The resolver gates reads on workspace membership (resource_resolver.rs
    // `is_member`), so the creator must be a member to resolve their own
    // resource. Idempotent — multiple resources in one workspace are fine.
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
            VALUES ($1, $2, 'owner') ON CONFLICT DO NOTHING",
    )
    .bind(workspace_id)
    .bind(creator)
    .execute(db)
    .await
    .expect("seed creator membership for resolver");

    sqlx::query(
        "INSERT INTO resources \
            (id, workspace_id, path, resource_type, display_name, latest_version, created_by) \
         VALUES ($1, $2, $3, $4, $5, 1, $6)",
    )
    .bind(resource_id)
    .bind(workspace_id)
    .bind(path)
    .bind(resource_type)
    .bind(path)
    .bind(creator)
    .execute(db)
    .await
    .expect("insert resources row");

    sqlx::query(
        "INSERT INTO resource_versions \
            (resource_id, version, vault_path, public_config, created_by) \
         VALUES ($1, 1, $2, $3, $4)",
    )
    .bind(resource_id)
    .bind(&vault_path)
    .bind(&public_config)
    .bind(creator)
    .execute(db)
    .await
    .expect("insert resource_versions row");

    resource_id
}

/// Insert an additional version for an existing resource. Bumps the parent
/// resource's `latest_version` so a follow-on `resolve` against the new
/// version sees a consistent join.
async fn seed_version(
    db: &PgPool,
    resource_id: Uuid,
    version: i32,
    creator: Uuid,
    public_config: serde_json::Value,
) {
    let vault_path: String = sqlx::query_scalar(
        "SELECT vault_path FROM resource_versions WHERE resource_id = $1 AND version = 1",
    )
    .bind(resource_id)
    .fetch_one(db)
    .await
    .expect("read base vault_path");
    // Reuse the workspace_id + resource_id prefix; only the trailing /v<n> changes.
    let new_vault_path = format!("{}v{version}", vault_path.trim_end_matches("v1"));

    sqlx::query(
        "INSERT INTO resource_versions \
            (resource_id, version, vault_path, public_config, created_by) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(resource_id)
    .bind(version)
    .bind(&new_vault_path)
    .bind(&public_config)
    .bind(creator)
    .execute(db)
    .await
    .expect("insert additional resource_versions row");

    sqlx::query("UPDATE resources SET latest_version = $1 WHERE id = $2")
        .bind(version)
        .bind(resource_id)
        .execute(db)
        .await
        .expect("bump latest_version");
}

/// Grant `permission` to `principal_id` on `resource_id`.
async fn grant_acl(
    db: &PgPool,
    resource_id: Uuid,
    principal_id: Uuid,
    permission: &str,
    granted_by: Uuid,
) {
    sqlx::query(
        "INSERT INTO resource_acl \
            (resource_id, principal_id, principal_kind, permission, granted_by) \
         VALUES ($1, $2, 'user', $3, $4)",
    )
    .bind(resource_id)
    .bind(principal_id)
    .bind(permission)
    .bind(granted_by)
    .execute(db)
    .await
    .expect("insert resource_acl row");
}

fn audit_ctx_for(principal_id: Uuid) -> AuditContext {
    AuditContext {
        instance_id: Some(Uuid::new_v4()),
        step_id: None,
        site: "test".to_string(),
        principal_id,
        action: AuditAction::Resolve,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

/// Happy path. One alias → one Postgres resource → envelope has both inline
/// public fields and the secret template ref.
#[tokio::test]
async fn resolve_returns_envelope_with_inline_public_and_secret_refs() {
    let db = common::create_test_db().await;
    let workspace_id = Uuid::new_v4();
    let principal_id = Uuid::new_v4();

    let resource_id = seed_resource(
        &db,
        workspace_id,
        principal_id,
        "postgres",
        "pg_main",
        json!({
            "host": "db.example.internal",
            "port": 5432,
            "database": "app",
            "username": "app_rw",
            "sslmode": "require"
        }),
    )
    .await;
    grant_acl(&db, resource_id, principal_id, "read", principal_id).await;

    let resolver = ResourceResolver::new(db.clone());
    let mut bindings = HashMap::new();
    bindings.insert(
        "db".to_string(),
        ResourcePin {
            resource_id,
            version: 1,
        },
    );

    let envelope = resolver
        .resolve(workspace_id, principal_id, &bindings, audit_ctx_for(principal_id))
        .await
        .expect("resolve must succeed");

    let db_subtree = envelope.get("db").expect("alias `db` in envelope");
    assert_eq!(db_subtree["host"], "db.example.internal");
    assert_eq!(db_subtree["port"], 5432);
    assert_eq!(db_subtree["database"], "app");
    assert_eq!(db_subtree["username"], "app_rw");
    assert_eq!(db_subtree["sslmode"], "require");

    let expected_template = format!(
        "{{{{secret:{}#password}}}}",
        vault_path_for(workspace_id, resource_id, 1)
    );
    assert_eq!(db_subtree["password"].as_str().unwrap(), expected_template);
}

/// One audit row per alias after a 2-alias resolve. Verifies the
/// `resource_audit` insert fires inside the transaction.
#[tokio::test]
async fn resolve_writes_one_audit_row_per_alias() {
    let db = common::create_test_db().await;
    let workspace_id = Uuid::new_v4();
    let principal_id = Uuid::new_v4();

    let pg_id = seed_resource(
        &db,
        workspace_id,
        principal_id,
        "postgres",
        "pg",
        json!({
            "host": "h", "port": 1, "database": "d", "username": "u", "sslmode": null
        }),
    )
    .await;
    let openai_id = seed_resource(
        &db,
        workspace_id,
        principal_id,
        "openai",
        "openai_main",
        json!({ "organization": "org-x" }),
    )
    .await;
    grant_acl(&db, pg_id, principal_id, "read", principal_id).await;
    grant_acl(&db, openai_id, principal_id, "read", principal_id).await;

    let resolver = ResourceResolver::new(db.clone());
    let mut bindings = HashMap::new();
    bindings.insert(
        "db".to_string(),
        ResourcePin {
            resource_id: pg_id,
            version: 1,
        },
    );
    bindings.insert(
        "ai".to_string(),
        ResourcePin {
            resource_id: openai_id,
            version: 1,
        },
    );

    resolver
        .resolve(workspace_id, principal_id, &bindings, audit_ctx_for(principal_id))
        .await
        .expect("two-alias resolve must succeed");

    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT resource_id, action FROM resource_audit ORDER BY id ASC",
    )
    .fetch_all(&db)
    .await
    .expect("read audit rows");
    assert_eq!(rows.len(), 2, "expected one audit row per alias");
    let resource_ids: Vec<Uuid> = rows.iter().map(|(rid, _)| *rid).collect();
    assert!(resource_ids.contains(&pg_id));
    assert!(resource_ids.contains(&openai_id));
    for (_, action) in &rows {
        assert_eq!(action, "resolve");
    }
}

/// Workspace-scoped access (v1 stopgap until `workspace_members` lands):
/// a principal who didn't create the resource and has no `resource_acl`
/// row may still resolve it, as long as the resource lives in the
/// workspace the caller is acting in. The audit row records the *actual*
/// caller, not the creator.
#[tokio::test]
async fn resolve_grants_workspace_scoped_read_without_acl_row() {
    let db = common::create_test_db().await;
    let workspace_id = Uuid::new_v4();
    let owner_id = Uuid::new_v4();
    let other_principal = Uuid::new_v4();

    let resource_id = seed_resource(
        &db,
        workspace_id,
        owner_id,
        "postgres",
        "shared",
        json!({ "host": "h", "port": 1, "database": "d", "username": "u" }),
    )
    .await;
    // `other_principal` gets a workspace membership row but NO `grant_acl`:
    // the point of this test is that workspace membership alone grants the
    // read — per-resource ACL rows are not consulted by the resolver in v1.
    sqlx::query(
        "INSERT INTO workspace_members (workspace_id, user_id, role) \
            VALUES ($1, $2, 'viewer') ON CONFLICT DO NOTHING",
    )
    .bind(workspace_id)
    .bind(other_principal)
    .execute(&db)
    .await
    .expect("seed other_principal membership");

    let resolver = ResourceResolver::new(db.clone());
    let mut bindings = HashMap::new();
    bindings.insert(
        "db".to_string(),
        ResourcePin {
            resource_id,
            version: 1,
        },
    );

    let envelope = resolver
        .resolve(
            workspace_id,
            other_principal,
            &bindings,
            audit_ctx_for(other_principal),
        )
        .await
        .expect("workspace-scoped read must succeed");
    assert!(envelope.get("db").is_some());

    let audit_principal: Uuid =
        sqlx::query_scalar("SELECT principal_id FROM resource_audit WHERE resource_id = $1")
            .bind(resource_id)
            .fetch_one(&db)
            .await
            .expect("audit row written");
    assert_eq!(
        audit_principal, other_principal,
        "audit must record the actual caller, not the creator"
    );
}

/// Workspace mismatch is still a hard denial — the workspace filter is
/// the v1 access gate, so resolving a resource that lives in a *different*
/// workspace from the one the caller is acting in must fail with
/// `ResourceNotFound` (workspace mismatch is intentionally
/// indistinguishable from soft-delete at the API surface).
#[tokio::test]
async fn resolve_wrong_workspace_returns_not_found() {
    let db = common::create_test_db().await;
    let owner_workspace = Uuid::new_v4();
    let other_workspace = Uuid::new_v4();
    let principal = Uuid::new_v4();

    let resource_id = seed_resource(
        &db,
        owner_workspace,
        principal,
        "postgres",
        "elsewhere",
        json!({ "host": "h", "port": 1, "database": "d", "username": "u" }),
    )
    .await;

    let resolver = ResourceResolver::new(db.clone());
    let mut bindings = HashMap::new();
    bindings.insert(
        "db".to_string(),
        ResourcePin {
            resource_id,
            version: 1,
        },
    );

    let err = resolver
        .resolve(other_workspace, principal, &bindings, audit_ctx_for(principal))
        .await
        .expect_err("cross-workspace read must fail");
    match err {
        ResolverError::ResourceNotFound { resource_id: rid } => assert_eq!(rid, resource_id),
        other => panic!("expected ResourceNotFound, got {other:?}"),
    }

    let count: i64 = sqlx::query("SELECT COUNT(*) FROM resource_audit")
        .fetch_one(&db)
        .await
        .expect("count audit rows")
        .get(0);
    assert_eq!(count, 0, "denied resolves must write no audit rows");
}

/// Unknown `resource_type` value in the DB row → `UnknownResourceType`.
/// Defends against a future migration that adds a type without updating the
/// shared registry.
#[tokio::test]
async fn resolve_unknown_resource_type_returns_error() {
    let db = common::create_test_db().await;
    let workspace_id = Uuid::new_v4();
    let principal_id = Uuid::new_v4();

    let resource_id = seed_resource(
        &db,
        workspace_id,
        principal_id,
        "definitely_not_real",
        "bogus",
        json!({}),
    )
    .await;
    // ACL grant skipped on purpose — we look up the descriptor before ACL so
    // an unknown type wins the race and yields UnknownResourceType, not
    // AclDenied. Test asserts that ordering.

    let resolver = ResourceResolver::new(db.clone());
    let mut bindings = HashMap::new();
    bindings.insert(
        "bogus".to_string(),
        ResourcePin {
            resource_id,
            version: 1,
        },
    );

    let err = resolver
        .resolve(workspace_id, principal_id, &bindings, audit_ctx_for(principal_id))
        .await
        .expect_err("unknown type must error");

    match err {
        ResolverError::UnknownResourceType { type_name } => {
            assert_eq!(type_name, "definitely_not_real");
        }
        other => panic!("expected UnknownResourceType, got {other:?}"),
    }
}

/// Pinning to an old version returns the old version's public_config —
/// proves rotation does not leak forward to instances pinned at v1.
#[tokio::test]
async fn resolve_pin_to_old_version_returns_old_public_config() {
    let db = common::create_test_db().await;
    let workspace_id = Uuid::new_v4();
    let principal_id = Uuid::new_v4();

    let resource_id = seed_resource(
        &db,
        workspace_id,
        principal_id,
        "postgres",
        "pg_rotated",
        json!({
            "host": "old.example.internal",
            "port": 5432,
            "database": "app",
            "username": "u_v1"
        }),
    )
    .await;
    grant_acl(&db, resource_id, principal_id, "read", principal_id).await;

    seed_version(
        &db,
        resource_id,
        2,
        principal_id,
        json!({
            "host": "new.example.internal",
            "port": 5432,
            "database": "app",
            "username": "u_v2"
        }),
    )
    .await;

    let resolver = ResourceResolver::new(db.clone());

    // Resolve pinned at v1.
    let mut pin_v1 = HashMap::new();
    pin_v1.insert(
        "db".to_string(),
        ResourcePin {
            resource_id,
            version: 1,
        },
    );
    let env_v1 = resolver
        .resolve(workspace_id, principal_id, &pin_v1, audit_ctx_for(principal_id))
        .await
        .expect("v1 resolve must succeed");
    assert_eq!(env_v1["db"]["host"], "old.example.internal");
    assert_eq!(env_v1["db"]["username"], "u_v1");

    // Sanity: v2 reads back the new config.
    let mut pin_v2 = HashMap::new();
    pin_v2.insert(
        "db".to_string(),
        ResourcePin {
            resource_id,
            version: 2,
        },
    );
    let env_v2 = resolver
        .resolve(workspace_id, principal_id, &pin_v2, audit_ctx_for(principal_id))
        .await
        .expect("v2 resolve must succeed");
    assert_eq!(env_v2["db"]["host"], "new.example.internal");
    assert_eq!(env_v2["db"]["username"], "u_v2");

    // The vault_path baked into the secret template encodes the version,
    // so v1 secret ref must differ from v2.
    let v1_password = env_v1["db"]["password"].as_str().unwrap();
    let v2_password = env_v2["db"]["password"].as_str().unwrap();
    assert!(v1_password.contains("/v1#password"), "v1 template was {v1_password}");
    assert!(v2_password.contains("/v2#password"), "v2 template was {v2_password}");
}
