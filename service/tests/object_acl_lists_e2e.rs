//! Object-ACL list-endpoint e2e — pins the two list behaviors the shared
//! helpers in `auth/grants.rs` implement:
//!
//! - `filter_and_annotate_visible` (assets, resources): a `restricted` row a
//!   member holds no grant for is DROPPED from the list; a grant (or ws
//!   Owner/Admin bypass) makes it visible with `my_effective_role` stamped.
//! - `annotate_roles_keep_all` (folders, templates, instances): rows stay in
//!   the list even when the caller can't open them — `my_effective_role` is
//!   simply null (tree navigation / list surfaces need the full structure).
//!
//! Requires the shared test infrastructure (`just -f
//! aithericon-test-infra/justfile up` — Postgres :5599 + NATS), same as
//! `workspace_acl_e2e.rs` / `resources_handlers.rs`. Uses the header-driven
//! mock authenticator so one app instance serves multiple users.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{self, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

use common::mock_auth::MockAuthenticator;
use common::test_app_with_authenticator;
use common::workspace_fixtures::{
    seed_member, seed_template_in_workspace, seed_workspace, subject_uuid,
};

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn req_as(subject: &str, workspace_id: Uuid) -> http::request::Builder {
    Request::builder()
        .header("cookie", "mekhan_session=valid")
        .header("x-test-subject", subject)
        .header("x-test-workspace", workspace_id.to_string())
}

async fn header_driven_app() -> (axum::Router, PgPool) {
    test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await
}

async fn get_as(app: &axum::Router, subject: &str, ws: Uuid, uri: &str) -> Value {
    let resp = app
        .clone()
        .oneshot(
            req_as(subject, ws)
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "GET {uri} as {subject}");
    body_json(resp.into_body()).await
}

/// Find the item with the given id in a JSON array, by its `id` field.
fn find_by_id<'a>(items: &'a [Value], id: Uuid) -> Option<&'a Value> {
    items
        .iter()
        .find(|v| v["id"].as_str() == Some(id.to_string().as_str()))
}

/// Seed an asset type + one asset directly (workspace scope), mirroring what
/// `POST /api/v1/assets` would persist. Returns the asset id.
async fn seed_asset(db: &PgPool, ws: Uuid, ref_key: &str, restricted: bool) -> Uuid {
    let type_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO asset_types (id, scope_kind, scope_id, name, display_name) \
              VALUES ($1, 'workspace', $2, $3, $3)",
    )
    .bind(type_id)
    .bind(ws)
    .bind(format!("{ref_key}_type"))
    .execute(db)
    .await
    .expect("seed asset type");

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO assets (id, scope_kind, scope_id, type_id, ref_key, display_name, restricted) \
              VALUES ($1, 'workspace', $2, $3, $4, $4, $5)",
    )
    .bind(id)
    .bind(ws)
    .bind(type_id)
    .bind(ref_key)
    .bind(restricted)
    .execute(db)
    .await
    .expect("seed asset");
    id
}

/// Seed a resource row directly (workspace scope). Returns the resource id.
async fn seed_resource(db: &PgPool, ws: Uuid, path: &str, restricted: bool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO resources \
            (id, workspace_id, scope_kind, scope_id, path, resource_type, display_name, \
             created_by, restricted) \
         VALUES ($1, $2, 'workspace', $2, $3, 'postgres', $3, $4, $5)",
    )
    .bind(id)
    .bind(ws)
    .bind(path)
    .bind(Uuid::new_v4())
    .bind(restricted)
    .execute(db)
    .await
    .expect("seed resource");
    id
}

/// Direct grant insert — the upsert shape `apply_grant` writes.
async fn seed_grant(db: &PgPool, ws: Uuid, kind: &str, object_id: Uuid, subject: &str, role: &str) {
    sqlx::query(
        "INSERT INTO object_grants (workspace_id, object_type, object_id, user_id, role, granted_by) \
              VALUES ($1, $2::object_kind, $3, $4, $5, $6)",
    )
    .bind(ws)
    .bind(kind)
    .bind(object_id)
    .bind(subject_uuid(subject))
    .bind(role)
    .bind(subject_uuid("alice"))
    .execute(db)
    .await
    .expect("seed grant");
}

// ---------------------------------------------------------------------------
// 1. Assets list — restricted rows are dropped without a grant, visible with
//    one, and always visible to a ws admin (filter_and_annotate_visible).
// ---------------------------------------------------------------------------
#[tokio::test]
async fn restricted_asset_hidden_until_granted() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(&db, &format!("ws-acl-assets-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;
    seed_member(&db, ws, "bob", "viewer").await;

    let open = seed_asset(&db, ws, "open_asset", false).await;
    let hidden = seed_asset(&db, ws, "hidden_asset", true).await;

    // Member without a grant: restricted asset is dropped; the open one is
    // annotated with the workspace floor.
    let body = get_as(&app, "bob", ws, "/api/v1/assets").await;
    let items = body["items"].as_array().unwrap();
    let open_row = find_by_id(items, open).expect("open asset visible to member");
    assert_eq!(open_row["my_effective_role"], "viewer");
    assert!(
        find_by_id(items, hidden).is_none(),
        "restricted asset must be hidden from a member without a grant"
    );

    // Viewer grant: the restricted asset appears with my_effective_role viewer.
    seed_grant(&db, ws, "asset", hidden, "bob", "viewer").await;
    let body = get_as(&app, "bob", ws, "/api/v1/assets").await;
    let items = body["items"].as_array().unwrap();
    let hidden_row = find_by_id(items, hidden).expect("granted restricted asset visible");
    assert_eq!(hidden_row["my_effective_role"], "viewer");

    // ws Owner/Admin bypass: alice always sees both.
    let body = get_as(&app, "alice", ws, "/api/v1/assets").await;
    let items = body["items"].as_array().unwrap();
    assert!(find_by_id(items, open).is_some());
    let hidden_row = find_by_id(items, hidden).expect("ws owner sees restricted asset");
    assert_eq!(hidden_row["my_effective_role"], "owner");
}

// ---------------------------------------------------------------------------
// 2. Resources list — same restricted-row filtering (one mirrored case).
// ---------------------------------------------------------------------------
#[tokio::test]
async fn restricted_resource_hidden_until_granted() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(&db, &format!("ws-acl-res-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;
    seed_member(&db, ws, "bob", "viewer").await;

    let open = seed_resource(&db, ws, "open_pg", false).await;
    let hidden = seed_resource(&db, ws, "hidden_pg", true).await;

    let uri = format!("/api/v1/resources?workspace_id={ws}");

    let body = get_as(&app, "bob", ws, &uri).await;
    let items = body["items"].as_array().unwrap();
    let open_row = find_by_id(items, open).expect("open resource visible to member");
    assert_eq!(open_row["my_effective_role"], "viewer");
    assert!(
        find_by_id(items, hidden).is_none(),
        "restricted resource must be hidden from a member without a grant"
    );

    seed_grant(&db, ws, "resource", hidden, "bob", "viewer").await;
    let body = get_as(&app, "bob", ws, &uri).await;
    let items = body["items"].as_array().unwrap();
    let hidden_row = find_by_id(items, hidden).expect("granted restricted resource visible");
    assert_eq!(hidden_row["my_effective_role"], "viewer");

    // ws admin bypass.
    let body = get_as(&app, "alice", ws, &uri).await;
    let items = body["items"].as_array().unwrap();
    assert!(find_by_id(items, hidden).is_some(), "ws owner sees all");
}

// ---------------------------------------------------------------------------
// 3. Keep-all pin — a restricted folder still appears in the folders list
//    (role null) and a template filed inside it still appears in the
//    templates list (role null). annotate_roles_keep_all must NOT filter.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn keep_all_surfaces_show_restricted_rows_with_null_role() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(&db, &format!("ws-acl-keep-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;
    seed_member(&db, ws, "bob", "viewer").await;

    // Folder via the API (path materialization), then flip it restricted.
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", ws)
                .method("POST")
                .uri(format!("/api/v1/workspaces/{ws}/folders"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "slug": "vault", "display_name": "Vault" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let folder_id: Uuid = body_json(resp.into_body()).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    sqlx::query("UPDATE folders SET restricted = TRUE WHERE id = $1")
        .bind(folder_id)
        .execute(&db)
        .await
        .expect("restrict folder");

    // Template filed inside the restricted folder.
    let tpl = seed_template_in_workspace(&db, ws, "filed-in-vault", "workspace").await;
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", ws)
                .method("PUT")
                .uri(format!("/api/v1/templates/{tpl}/folder"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "folder_id": folder_id.to_string() }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let tpl_open = seed_template_in_workspace(&db, ws, "out-in-the-open", "workspace").await;

    // Folders list: the restricted folder is PRESENT for bob with role null
    // (tree navigation must see the full path structure).
    let body = get_as(&app, "bob", ws, &format!("/api/v1/workspaces/{ws}/folders")).await;
    let folders = body.as_array().unwrap();
    let vault = find_by_id(folders, folder_id).expect("restricted folder stays in the tree");
    assert!(
        vault["my_effective_role"].is_null(),
        "no grant ⇒ null annotation, got {}",
        vault["my_effective_role"]
    );

    // Templates list: the filed template is PRESENT for bob with role null;
    // the open one carries the workspace floor.
    let body = get_as(&app, "bob", ws, "/api/v1/templates").await;
    let items = body["items"].as_array().unwrap();
    let filed = find_by_id(items, tpl).expect("template in restricted folder stays listed");
    assert!(
        filed["my_effective_role"].is_null(),
        "no grant ⇒ null annotation, got {}",
        filed["my_effective_role"]
    );
    let open = find_by_id(items, tpl_open).expect("open template listed");
    assert_eq!(open["my_effective_role"], "viewer");

    // ws owner sees real roles on both surfaces.
    let body = get_as(&app, "alice", ws, &format!("/api/v1/workspaces/{ws}/folders")).await;
    let vault = find_by_id(body.as_array().unwrap(), folder_id).unwrap();
    assert_eq!(vault["my_effective_role"], "owner");
}
