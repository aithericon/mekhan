//! Phase B integration tests: per-project OpenAPI bundle, email→subject
//! resolver, active-workspace cookie switching, and demos auto-membership.
//!
//! Requires the shared test infrastructure (Postgres at :5599, NATS at
//! :4322). Uses the header-driven mock authenticator so multiple synthetic
//! users can drive a single app instance.

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
use common::workspace_fixtures::{seed_member, seed_workspace, subject_uuid};

const ACTIVE_WS_COOKIE: &str = "mekhan_active_workspace";

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn req_as(subject: &str, workspace_id: Option<Uuid>) -> http::request::Builder {
    let mut b = Request::builder().header("cookie", "mekhan_session=valid");
    b = b.header("x-test-subject", subject);
    if let Some(ws) = workspace_id {
        b = b.header("x-test-workspace", ws.to_string());
    }
    b
}

/// Like `req_as` but also stamps an active-workspace cookie.
fn req_as_with_active(
    subject: &str,
    workspace_id: Option<Uuid>,
    active: Uuid,
) -> http::request::Builder {
    let mut b = Request::builder().header(
        "cookie",
        format!("mekhan_session=valid; {ACTIVE_WS_COOKIE}={active}"),
    );
    b = b.header("x-test-subject", subject);
    if let Some(ws) = workspace_id {
        b = b.header("x-test-workspace", ws.to_string());
    }
    b
}

async fn header_driven_app() -> (axum::Router, PgPool) {
    test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await
}

/// Seed a publishable template with a webhook trigger node in the
/// workspace. Returns the template id.
async fn seed_template_with_webhook(
    db: &PgPool,
    workspace_id: Uuid,
    name: &str,
    webhook_slug: &str,
    method: &str,
    target_fields: &[&str],
) -> Uuid {
    let id = Uuid::new_v4();
    let payload_mapping: Vec<Value> = target_fields
        .iter()
        .map(|f| json!({ "targetField": f, "expression": format!("payload.{f}") }))
        .collect();
    let graph = json!({
        "nodes": [
            {
                "id": "trig1",
                "node_type": "trigger",
                "data": {
                    "type": "trigger",
                    "label": format!("{name} webhook"),
                    "description": "Test webhook",
                    "source": {
                        "kind": "webhook",
                        "slug": webhook_slug,
                        "auth": { "kind": "shared_secret", "header": "X-Webhook-Token", "secret_ref": "test" },
                        "requireMethod": method,
                    },
                    "payloadMapping": payload_mapping,
                    "enabled": true,
                }
            },
            { "id": "start", "node_type": "start", "data": { "type": "start" } }
        ],
        "edges": []
    });
    sqlx::query(
        "INSERT INTO workflow_templates \
            (id, name, description, version, is_latest, graph, author_id, workspace_id, visibility, published, interface_json) \
         VALUES ($1, $2, '', 1, TRUE, $3, $4, $5, 'workspace', TRUE, NULL)",
    )
    .bind(id)
    .bind(name)
    .bind(&graph)
    .bind(subject_uuid("seeder"))
    .bind(workspace_id)
    .execute(db)
    .await
    .expect("seed template with webhook");
    id
}

async fn create_folder(app: &axum::Router, subject: &str, workspace_id: Uuid, slug: &str) -> Uuid {
    let resp = app
        .clone()
        .oneshot(
            req_as(subject, Some(workspace_id))
                .method("POST")
                .uri(format!("/api/v1/workspaces/{workspace_id}/folders"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "slug": slug, "display_name": slug, "description": "test" })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create_folder");
    let body = body_json(resp.into_body()).await;
    Uuid::parse_str(body["id"].as_str().unwrap()).unwrap()
}

/// File a template into a folder (its single home) via `PUT /templates/{id}/folder`.
async fn set_template_folder(
    app: &axum::Router,
    subject: &str,
    workspace_id: Uuid,
    folder_id: Uuid,
    template_id: Uuid,
) {
    let resp = app
        .clone()
        .oneshot(
            req_as(subject, Some(workspace_id))
                .method("PUT")
                .uri(format!("/api/v1/templates/{template_id}/folder"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "folder_id": folder_id }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT, "set_template_folder");
}

// -- B1: per-project OpenAPI bundle ------------------------------------------

#[tokio::test]
async fn openapi_bundle_lists_attached_webhooks() {
    let (app, db) = header_driven_app().await;
    let ws = seed_workspace(&db, &format!("ws-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;

    let tpl = seed_template_with_webhook(
        &db,
        ws,
        "Invoices",
        "invoice-received",
        "POST",
        &["invoice_id", "amount"],
    )
    .await;
    let folder = create_folder(&app, "alice", ws, "billing").await;
    set_template_folder(&app, "alice", ws, folder, tpl).await;

    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("GET")
                .uri(format!(
                    "/api/v1/workspaces/{ws}/folders/{folder}/openapi.json"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let doc = body_json(resp.into_body()).await;

    assert_eq!(doc["openapi"], "3.0.3");
    assert_eq!(doc["info"]["title"], "Folder: billing");
    let path = &doc["paths"]["/api/triggers/webhook/invoice-received"];
    assert!(path["post"].is_object(), "POST operation must exist");
    let op = &path["post"];
    assert_eq!(op["operationId"], "webhook_invoice-received");
    assert!(op["tags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "webhooks"));
    let auth_param = op["parameters"][0].clone();
    assert_eq!(auth_param["name"], "X-Webhook-Token");

    let schema_name = "Webhook_invoice-received";
    let schema = &doc["components"]["schemas"][schema_name];
    let props = schema["properties"].as_object().expect("hinted props");
    assert!(props.contains_key("invoice_id"));
    assert!(props.contains_key("amount"));
}

#[tokio::test]
async fn openapi_bundle_rejects_non_member() {
    let (app, db) = header_driven_app().await;
    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("ws-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;
    seed_member(&db, ws_b, "bob", "owner").await;

    let folder = create_folder(&app, "alice", ws_a, "billing").await;

    let resp = app
        .clone()
        .oneshot(
            req_as("bob", Some(ws_b))
                .method("GET")
                .uri(format!(
                    "/api/v1/workspaces/{ws_a}/folders/{folder}/openapi.json"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn openapi_bundle_404_on_project_in_wrong_workspace() {
    let (app, db) = header_driven_app().await;
    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("ws-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;
    seed_member(&db, ws_b, "alice", "owner").await;

    let folder_in_a = create_folder(&app, "alice", ws_a, "billing").await;

    // Alice is a member of ws_b, so the gate passes; the folder belongs
    // to ws_a, so the in-workspace check should 404.
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws_b))
                .method("GET")
                .uri(format!(
                    "/api/v1/workspaces/{ws_b}/folders/{folder_in_a}/openapi.json"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn openapi_bundle_empty_project_yields_empty_paths() {
    let (app, db) = header_driven_app().await;
    let ws = seed_workspace(&db, &format!("ws-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;
    let folder = create_folder(&app, "alice", ws, "empty").await;

    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("GET")
                .uri(format!(
                    "/api/v1/workspaces/{ws}/folders/{folder}/openapi.json"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let doc = body_json(resp.into_body()).await;
    assert_eq!(doc["paths"].as_object().unwrap().len(), 0);
}

#[tokio::test]
async fn openapi_bundle_method_defaults_to_post_when_unset() {
    let (app, db) = header_driven_app().await;
    let ws = seed_workspace(&db, &format!("ws-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;

    let id = Uuid::new_v4();
    let graph = json!({
        "nodes": [{
            "id": "trig1",
            "node_type": "trigger",
            "data": {
                "type": "trigger",
                "label": "ping",
                "source": { "kind": "webhook", "slug": "ping-handler", "auth": { "kind": "none" } },
                "payloadMapping": [],
                "enabled": true,
            }
        }]
    });
    sqlx::query(
        "INSERT INTO workflow_templates (id, name, description, version, is_latest, graph, author_id, workspace_id, visibility, published) \
             VALUES ($1, 'Ping', '', 1, TRUE, $2, $3, $4, 'workspace', TRUE)",
    )
    .bind(id)
    .bind(&graph)
    .bind(subject_uuid("seeder"))
    .bind(ws)
    .execute(&db)
    .await
    .unwrap();

    let folder = create_folder(&app, "alice", ws, "ping").await;
    set_template_folder(&app, "alice", ws, folder, id).await;

    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("GET")
                .uri(format!(
                    "/api/v1/workspaces/{ws}/folders/{folder}/openapi.json"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let doc = body_json(resp.into_body()).await;
    assert!(doc["paths"]["/api/triggers/webhook/ping-handler"]["post"].is_object());
    assert!(doc["paths"]["/api/triggers/webhook/ping-handler"]["get"].is_null());
}

// -- B2: email → subject resolver --------------------------------------------

#[tokio::test]
async fn email_resolver_dev_noop_echoes_email_as_subject() {
    let (app, db) = header_driven_app().await;
    let ws = seed_workspace(&db, &format!("ws-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;

    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("POST")
                .uri("/api/v1/users/resolve")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "email": "bob@corp.com" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["subject"], "bob@corp.com");
    assert_eq!(body["email"], "bob@corp.com");
}

#[tokio::test]
async fn email_resolver_rejects_invalid_input() {
    let (app, db) = header_driven_app().await;
    let ws = seed_workspace(&db, &format!("ws-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;

    for bad in ["", "   ", "not-an-email"].iter() {
        let resp = app
            .clone()
            .oneshot(
                req_as("alice", Some(ws))
                    .method("POST")
                    .uri("/api/v1/users/resolve")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "email": bad }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "rejected '{bad}'");
    }
}

#[tokio::test]
async fn email_resolver_end_to_end_with_member_add() {
    let (app, db) = header_driven_app().await;
    let ws = seed_workspace(&db, &format!("ws-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;

    // 1. Alice resolves Bob's email.
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("POST")
                .uri("/api/v1/users/resolve")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "email": "bob@corp.com" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let subject = body_json(resp.into_body()).await["subject"]
        .as_str()
        .unwrap()
        .to_string();

    // 2. Alice adds Bob as a member using the resolved subject.
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("POST")
                .uri(format!("/api/v1/workspaces/{ws}/members"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "subject": subject, "role": "editor" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "add bob as editor");

    // 3. Bob (subject == "bob@corp.com") can now list members of ws.
    let resp = app
        .clone()
        .oneshot(
            req_as("bob@corp.com", Some(ws))
                .method("GET")
                .uri(format!("/api/v1/workspaces/{ws}/members"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let members = body_json(resp.into_body()).await;
    let roles: Vec<&str> = members
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap_or(""))
        .collect();
    assert!(roles.contains(&"editor"), "bob's editor row present");
}

// -- B3: active workspace cookie ---------------------------------------------

#[tokio::test]
async fn active_workspace_cookie_overrides_resolver_pick() {
    let (app, db) = header_driven_app().await;
    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("ws-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;
    seed_member(&db, ws_b, "alice", "owner").await;

    // Without cookie — the header-supplied workspace is honored.
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws_a))
                .method("GET")
                .uri("/api/auth/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["workspace_id"].as_str().unwrap(), ws_a.to_string());

    // With the active-workspace cookie pointing at ws_b — override wins.
    let resp = app
        .clone()
        .oneshot(
            req_as_with_active("alice", Some(ws_a), ws_b)
                .method("GET")
                .uri("/api/auth/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["workspace_id"].as_str().unwrap(), ws_b.to_string());
}

#[tokio::test]
async fn active_workspace_cookie_ignored_when_not_member() {
    let (app, db) = header_driven_app().await;
    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("ws-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;
    // Alice is NOT a member of ws_b.

    let resp = app
        .clone()
        .oneshot(
            req_as_with_active("alice", Some(ws_a), ws_b)
                .method("GET")
                .uri("/api/auth/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    // The override silently degrades to the header-supplied default,
    // never granting access to ws_b.
    assert_eq!(body["workspace_id"].as_str().unwrap(), ws_a.to_string());
}

#[tokio::test]
async fn active_workspace_cookie_ignored_when_malformed() {
    let (app, db) = header_driven_app().await;
    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .header(
                    "cookie",
                    "mekhan_session=valid; mekhan_active_workspace=not-a-uuid",
                )
                .header("x-test-subject", "alice")
                .header("x-test-workspace", ws_a.to_string())
                .method("GET")
                .uri("/api/auth/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["workspace_id"].as_str().unwrap(), ws_a.to_string());
}

#[tokio::test]
async fn set_active_workspace_emits_cookie_and_204() {
    let (app, db) = header_driven_app().await;
    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("ws-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;
    seed_member(&db, ws_b, "alice", "owner").await;

    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws_a))
                .method("POST")
                .uri("/api/v1/me/active-workspace")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "workspace_id": ws_b }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .expect("Set-Cookie header emitted");
    let s = set_cookie.to_str().unwrap();
    assert!(
        s.contains(&format!("mekhan_active_workspace={ws_b}")),
        "cookie carries ws_b: {s}"
    );
    assert!(s.contains("HttpOnly"));
}

#[tokio::test]
async fn set_active_workspace_rejects_non_member() {
    let (app, db) = header_driven_app().await;
    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("ws-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;
    // No membership in ws_b.

    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws_a))
                .method("POST")
                .uri("/api/v1/me/active-workspace")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "workspace_id": ws_b }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn clear_active_workspace_emits_removal_cookie() {
    let (app, db) = header_driven_app().await;
    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;

    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws_a))
                .method("DELETE")
                .uri("/api/v1/me/active-workspace")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let set_cookie = resp
        .headers()
        .get("set-cookie")
        .expect("Set-Cookie emitted");
    let s = set_cookie.to_str().unwrap();
    // Removal cookies set a far-past expiry and zero max-age.
    assert!(s.contains("mekhan_active_workspace="));
    assert!(
        s.contains("Max-Age=0") || s.contains("expires=Thu, 01 Jan 1970"),
        "removal cookie: {s}"
    );
}

// -- Demos auto-membership ---------------------------------------------------

#[tokio::test]
async fn list_templates_in_demos_workspace_includes_seeded_demos_for_member() {
    // Demos workspace is seeded by migration 20240123 with is_system=TRUE,
    // and the resolver auto-adds every user as a viewer. Verify the
    // membership row is present (the resolver runs at session resolution;
    // the mock authenticator doesn't run it, so we replicate that path by
    // inserting the membership directly — the actual auto-provision is
    // covered by a separate test below).
    let (app, db) = header_driven_app().await;

    let (demos_id,): (Uuid,) = sqlx::query_as("SELECT id FROM workspaces WHERE slug = 'demos'")
        .fetch_one(&db)
        .await
        .expect("demos workspace seeded by migrations");

    seed_member(&db, demos_id, "newbie", "viewer").await;

    // newbie can list workspaces and see demos in the list.
    let resp = app
        .clone()
        .oneshot(
            req_as("newbie", Some(demos_id))
                .method("GET")
                .uri("/api/v1/workspaces")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let slugs: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["slug"].as_str().unwrap_or(""))
        .collect();
    assert!(
        slugs.contains(&"demos"),
        "demos in workspaces list: {slugs:?}"
    );
}

#[tokio::test]
async fn resolver_auto_provisions_demos_membership() {
    // Wire the real DbPrincipalResolver (not the mock auth) and prove it
    // adds the caller to the demos workspace on first resolve.
    use mekhan_service::auth::resolver::DbPrincipalResolver;
    use mekhan_service::auth::{PrincipalResolver, VerifiedClaims};

    let db = common::create_test_db().await;
    let resolver = DbPrincipalResolver::new(db.clone());
    let claims = VerifiedClaims {
        subject: "fresh-user".to_string(),
        issuer: "test".to_string(),
        audience: vec!["mekhan".into()],
        expires_at: i64::MAX,
        extra: Default::default(),
    };
    let user = resolver.resolve(claims).await.expect("resolve");

    // Membership row should now exist for the seeded demos workspace.
    let (demos_id,): (Uuid,) = sqlx::query_as("SELECT id FROM workspaces WHERE slug = 'demos'")
        .fetch_one(&db)
        .await
        .unwrap();
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(demos_id)
    .bind(user.subject_as_uuid())
    .fetch_optional(&db)
    .await
    .unwrap();
    assert_eq!(row.map(|(r,)| r), Some("viewer".to_string()));
}

// -- B4: read a single template's tags (powers the settings editor) ----------

/// Set tags via PUT, then read them back via GET — the editor opens by
/// fetching the current set so a full-replace PUT doesn't clobber labels the
/// author can't see.
#[tokio::test]
async fn get_template_tags_round_trips_after_put() {
    let (app, db) = header_driven_app().await;
    let ws = seed_workspace(&db, &format!("ws-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;
    let tid = seed_template_with_webhook(&db, ws, "tagged", "hook", "POST", &["x"]).await;

    // PUT in unsorted order with a blank that must be dropped.
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("PUT")
                .uri(format!("/api/v1/templates/{tid}/tags"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "tags": ["beta", "alpha", "  "] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "put tags");

    // GET returns them sorted, blank dropped.
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("GET")
                .uri(format!("/api/v1/templates/{tid}/tags"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "get tags");
    let body = body_json(resp.into_body()).await;
    let tags: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap())
        .collect();
    assert_eq!(tags, vec!["alpha", "beta"]);
}

/// The GET read gate composes with `can_read_template`: a non-member is
/// rejected on a workspace-scoped template but allowed once it's public.
#[tokio::test]
async fn get_template_tags_honors_read_gate() {
    let (app, db) = header_driven_app().await;
    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("ws-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;
    seed_member(&db, ws_b, "bob", "owner").await;
    let tid = seed_template_with_webhook(&db, ws_a, "secret", "hook", "POST", &["x"]).await;

    // Alice tags it.
    app.clone()
        .oneshot(
            req_as("alice", Some(ws_a))
                .method("PUT")
                .uri(format!("/api/v1/templates/{tid}/tags"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "tags": ["confidential"] }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    // Bob (member of ws_b only) can't read tags while it's workspace-scoped.
    let resp = app
        .clone()
        .oneshot(
            req_as("bob", Some(ws_b))
                .method("GET")
                .uri(format!("/api/v1/templates/{tid}/tags"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "non-member rejected");

    // Alice flips it public (admin-gated; she's owner).
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws_a))
                .method("PATCH")
                .uri(format!("/api/v1/templates/{tid}/visibility"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "visibility": "public" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT, "flip public");

    // Now Bob's read succeeds and sees the tag.
    let resp = app
        .clone()
        .oneshot(
            req_as("bob", Some(ws_b))
                .method("GET")
                .uri(format!("/api/v1/templates/{tid}/tags"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "public read allowed");
    let body = body_json(resp.into_body()).await;
    assert_eq!(body, json!(["confidential"]));
}

/// Regression for the deployed-instance 403: a fresh principal must be
/// auto-provisioned as an EDITOR of the `default` workspace on first resolve,
/// not just a viewer of `demos`. Without this, real users can't edit the
/// templates migration 20240124 moved into `default`.
#[tokio::test]
async fn resolver_auto_provisions_default_membership_as_editor() {
    use mekhan_service::auth::resolver::DbPrincipalResolver;
    use mekhan_service::auth::{PrincipalResolver, VerifiedClaims};

    let db = common::create_test_db().await;
    let resolver = DbPrincipalResolver::new(db.clone());
    let claims = VerifiedClaims {
        subject: "fresh-editor".to_string(),
        issuer: "test".to_string(),
        audience: vec!["mekhan".into()],
        expires_at: i64::MAX,
        extra: Default::default(),
    };
    let user = resolver.resolve(claims).await.expect("resolve");

    let (default_id,): (Uuid,) = sqlx::query_as("SELECT id FROM workspaces WHERE slug = 'default'")
        .fetch_one(&db)
        .await
        .unwrap();
    let role: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM workspace_members WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(default_id)
    .bind(user.subject_as_uuid())
    .fetch_optional(&db)
    .await
    .unwrap();
    assert_eq!(role.map(|(r,)| r), Some("editor".to_string()));

    // And the picked active workspace prefers `default` over `demos`.
    assert_eq!(user.workspace_id, Some(default_id));
}
