//! Cross-workspace ACL e2e — proves Phase A1 + A2 wire up consistently.
//!
//! Requires the shared test infrastructure (`just -f aithericon-test-infra/justfile up`).
//! Uses the header-driven mock authenticator so a single app instance can
//! drive requests as multiple users.

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

fn req_as(subject: &str, workspace_id: Option<Uuid>) -> http::request::Builder {
    let mut b = Request::builder().header("cookie", "mekhan_session=valid");
    b = b.header("x-test-subject", subject);
    if let Some(ws) = workspace_id {
        b = b.header("x-test-workspace", ws.to_string());
    }
    b
}

async fn header_driven_app() -> (axum::Router, PgPool) {
    test_app_with_authenticator(Arc::new(MockAuthenticator::header_driven())).await
}

// ---------------------------------------------------------------------------
// 1. Two users in two workspaces are isolated on list + read
// ---------------------------------------------------------------------------
#[tokio::test]
async fn two_users_two_workspaces_isolated() {
    let (app, db) = header_driven_app().await;

    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("ws-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;
    seed_member(&db, ws_b, "bob", "owner").await;

    let tpl_a = seed_template_in_workspace(&db, ws_a, "alice's secret", "workspace").await;

    // Bob's list does not include alice's template
    let resp = app
        .clone()
        .oneshot(
            req_as("bob", Some(ws_b))
                .method("GET")
                .uri("/api/v1/templates")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let names: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or(""))
        .collect();
    assert!(
        !names.contains(&"alice's secret"),
        "bob should not see alice's template in his workspace list"
    );

    // Bob GET on the id directly → 403
    let resp = app
        .clone()
        .oneshot(
            req_as("bob", Some(ws_b))
                .method("GET")
                .uri(format!("/api/v1/templates/{tpl_a}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// 2. visibility = 'public' lets cross-workspace users read but not write
// ---------------------------------------------------------------------------
#[tokio::test]
async fn public_visibility_crosses_workspace() {
    let (app, db) = header_driven_app().await;

    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("ws-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;
    seed_member(&db, ws_b, "bob", "owner").await;

    let tpl_a = seed_template_in_workspace(&db, ws_a, "the public one", "public").await;

    // Bob reads it (200)
    let resp = app
        .clone()
        .oneshot(
            req_as("bob", Some(ws_b))
                .method("GET")
                .uri(format!("/api/v1/templates/{tpl_a}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Bob's PUT (update) is rejected — public is read-only across workspaces.
    let resp = app
        .clone()
        .oneshot(
            req_as("bob", Some(ws_b))
                .method("PUT")
                .uri(format!("/api/v1/templates/{tpl_a}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": "bob's hijack" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// 3. Demos workspace is system-owned + public — visible to non-members
// ---------------------------------------------------------------------------
#[tokio::test]
async fn demo_workspace_is_visible_to_authenticated_non_members() {
    let (app, db) = header_driven_app().await;

    // Seed a public template owned by the existing demos workspace.
    // Migration 20240123 inserted '00000000-0000-0000-0000-0000000000de'.
    let demo_ws: Uuid = "00000000-0000-0000-0000-0000000000de".parse().unwrap();
    let _ = seed_template_in_workspace(&db, demo_ws, "fake demo", "public").await;

    // A user who isn't a member of the demos workspace still sees it via
    // visibility = 'public'.
    let ws_other = seed_workspace(&db, &format!("ws-other-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_other, "carol", "owner").await;

    let resp = app
        .oneshot(
            req_as("carol", Some(ws_other))
                .method("GET")
                .uri("/api/v1/templates")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let names: Vec<String> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        names.iter().any(|n| n == "fake demo"),
        "expected the public demo template in carol's list, got {names:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. Admin adds a member by OIDC subject; role lookup uses subject_as_uuid
// ---------------------------------------------------------------------------
#[tokio::test]
async fn admin_adds_member_by_subject() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(&db, &format!("ws-admin-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;

    // Alice (owner == admin-or-above) adds bob as editor.
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("POST")
                .uri(format!("/api/v1/workspaces/{ws}/members"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "subject": "bob", "role": "editor" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["role"], "editor");
    assert_eq!(
        body["user_id"].as_str().unwrap(),
        subject_uuid("bob").to_string()
    );

    // Roster reflects it.
    let resp = app
        .oneshot(
            req_as("alice", Some(ws))
                .method("GET")
                .uri(format!("/api/v1/workspaces/{ws}/members"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2, "expected 2 members, got {arr:?}");
}

// ---------------------------------------------------------------------------
// 5. Last-owner removal is refused with 409
// ---------------------------------------------------------------------------
#[tokio::test]
async fn cannot_remove_last_owner() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(&db, &format!("ws-solo-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;

    let alice_uuid = subject_uuid("alice");
    let resp = app
        .oneshot(
            req_as("alice", Some(ws))
                .method("DELETE")
                .uri(format!("/api/v1/workspaces/{ws}/members/{alice_uuid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// 6. Project attach filter — list_templates?project_id=X returns only attached
// ---------------------------------------------------------------------------
#[tokio::test]
async fn project_attach_filters_listing() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(&db, &format!("ws-proj-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;

    let tpl_in = seed_template_in_workspace(&db, ws, "in-project", "workspace").await;
    let _tpl_out = seed_template_in_workspace(&db, ws, "out-of-project", "workspace").await;

    // Create a project
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("POST")
                .uri(format!("/api/v1/workspaces/{ws}/projects"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "slug": "q4", "display_name": "Q4" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp.into_body()).await;
    let project_id = body["id"].as_str().unwrap().to_string();

    // Attach tpl_in
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("POST")
                .uri(format!("/api/v1/projects/{project_id}/templates"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "template_id": tpl_in.to_string() }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Filter listing by project
    let resp = app
        .oneshot(
            req_as("alice", Some(ws))
                .method("GET")
                .uri(format!("/api/v1/templates?project_id={project_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let names: Vec<String> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(names.contains(&"in-project".to_string()));
    assert!(!names.contains(&"out-of-project".to_string()));
}

// ---------------------------------------------------------------------------
// 7. Tag filter — `?tag=foo` narrows the listing
// ---------------------------------------------------------------------------
#[tokio::test]
async fn tag_filter_narrows_listing() {
    let (app, db) = header_driven_app().await;

    let ws = seed_workspace(&db, &format!("ws-tags-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws, "alice", "owner").await;

    let tpl_foo = seed_template_in_workspace(&db, ws, "foo-tagged", "workspace").await;
    let _tpl_bar = seed_template_in_workspace(&db, ws, "bar-tagged", "workspace").await;

    // Tag foo
    let resp = app
        .clone()
        .oneshot(
            req_as("alice", Some(ws))
                .method("PUT")
                .uri(format!("/api/v1/templates/{tpl_foo}/tags"))
                .header("content-type", "application/json")
                .body(Body::from(json!({ "tags": ["foo"] }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            req_as("alice", Some(ws))
                .method("GET")
                .uri("/api/v1/templates?tag=foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let names: Vec<String> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(names.contains(&"foo-tagged".to_string()));
    assert!(!names.contains(&"bar-tagged".to_string()));
}

// ---------------------------------------------------------------------------
// 8. Petri proxy — cross-workspace instance access rejected before engine dial
// ---------------------------------------------------------------------------
#[tokio::test]
async fn petri_proxy_rejects_cross_workspace_instance() {
    // The proxy gate runs before the upstream HTTP dial — verified by
    // pointing the proxy at a port we don't bind. If the gate didn't fire,
    // the result would be 502 (engine unreachable); with the gate, it's 403.
    let (app, db) = test_app_with_authenticator_and_petri_url(
        Arc::new(MockAuthenticator::header_driven()),
        "http://127.0.0.1:1", // unreachable
    )
    .await;

    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("ws-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;
    seed_member(&db, ws_b, "bob", "owner").await;

    let tpl_a = seed_template_in_workspace(&db, ws_a, "alice's", "workspace").await;

    // Wire up a workflow_instances row pointing at tpl_a so the proxy can
    // resolve net_id → workspace.
    let net_id = format!("mekhan-{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO workflow_instances (id, net_id, template_id, template_version, status, created_by) \
              VALUES ($1, $2, $3, 1, 'running', $4)",
    )
    .bind(Uuid::new_v4())
    .bind(&net_id)
    .bind(tpl_a)
    .bind(Uuid::new_v4())
    .execute(&db)
    .await
    .expect("seed instance row");

    let resp = app
        .oneshot(
            req_as("bob", Some(ws_b))
                .method("GET")
                .uri(format!("/petri/api/nets/{net_id}/state"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "cross-workspace petri proxy access should 403 before dialing the engine"
    );
}

/// Build an app whose proxy targets a caller-supplied URL while keeping
/// header-driven auth. Lets the proxy-gate test point at an unreachable
/// host so a missed gate would surface as 502, not 403.
async fn test_app_with_authenticator_and_petri_url(
    authenticator: Arc<dyn mekhan_service::auth::Authenticator>,
    petri_url: &str,
) -> (axum::Router, PgPool) {
    use mekhan_service::auth::bff::session::{PgSessionStore, SessionStore};
    use mekhan_service::auth::dev::NoopTokenVerifier;
    use mekhan_service::auth::resolver::StaticPrincipalResolver;
    use mekhan_service::catalogue::repository::PgCatalogueRepository;
    use mekhan_service::causality::live::LiveBroadcasts;
    use mekhan_service::config::AppConfig;
    use mekhan_service::nats::MekhanNats;
    use mekhan_service::petri::client::PetriClient;
    use mekhan_service::s3::ArtifactStore;
    use mekhan_service::triggers::TriggerDispatcher;
    use mekhan_service::yjs::manager::YjsManager;
    use mekhan_service::yjs::persistence::YjsPersistence;
    use mekhan_service::{build_router, AppState};

    let db = common::create_test_db().await;
    let mut config: AppConfig = common::test_config();
    config.petri_lab_url = petri_url.to_string();
    let petri = PetriClient::new(petri_url);
    let nats = MekhanNats::connect(&config.nats_url, None)
        .await
        .expect("test NATS");
    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs = Arc::new(YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));
    let triggers = Arc::new(TriggerDispatcher::new(db.clone(), petri.clone(), nats.clone()));

    let state = AppState {
        db: db.clone(),
        petri,
        nats,
        config,
        yjs,
        s3: artifact_store,
        artifact_s3: None,
        catalogue_repo: Arc::new(PgCatalogueRepository::new(db.clone())),
        live: LiveBroadcasts::new(),
        authenticator,
        session_store,
        oidc: None,
        token_verifier: Arc::new(NoopTokenVerifier::default()),
        principal_resolver: Arc::new(StaticPrincipalResolver),
        introspection: None,
        zitadel_mgmt: None,
        triggers,
        result_waiters: mekhan_service::triggers::ResultWaiters::new(),
        resource_store: Arc::new(aithericon_resources::InMemoryResourceStore::new()),
        resource_resolver: Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
    };
    (build_router(state), db)
}

// ---------------------------------------------------------------------------
// 9. Yjs WS — cross-workspace upgrade rejected before WS handshake
// ---------------------------------------------------------------------------
#[tokio::test]
async fn yjs_ws_rejects_cross_workspace() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    // Real TCP server + real WS handshake — synthetic `oneshot()` requests
    // can't drive HTTP/1.1 upgrade through Tower, so the `WebSocketUpgrade`
    // extractor 426s before the gate ever runs.
    let (addr, db) = common::start_test_server_with_authenticator(Arc::new(
        MockAuthenticator::header_driven(),
    ))
    .await;

    let ws_a = seed_workspace(&db, &format!("ws-a-{}", Uuid::new_v4().simple())).await;
    let ws_b = seed_workspace(&db, &format!("ws-b-{}", Uuid::new_v4().simple())).await;
    seed_member(&db, ws_a, "alice", "owner").await;
    seed_member(&db, ws_b, "bob", "owner").await;
    let tpl = seed_template_in_workspace(&db, ws_a, "alice-only", "workspace").await;

    let url = format!("ws://{addr}/api/yjs/{tpl}");
    let mut req = url.into_client_request().expect("ws request");
    req.headers_mut().insert(
        "x-test-subject",
        http::HeaderValue::from_static("bob"),
    );
    req.headers_mut().insert(
        "x-test-workspace",
        http::HeaderValue::from_str(&ws_b.to_string()).unwrap(),
    );
    // The Yjs auth path requires the session cookie just to be present
    // (the cookie value is ignored by the header-driven mock).
    req.headers_mut().insert(
        "cookie",
        http::HeaderValue::from_static("mekhan_session=valid"),
    );

    let result = tokio_tungstenite::connect_async(req).await;
    let err = result.expect_err("cross-workspace WS upgrade should be rejected");
    // tokio-tungstenite surfaces HTTP rejection as `Http(response)` with the
    // server's status. Anything in the 4xx range proves the upgrade was
    // refused before WS framing started; we assert specifically 403.
    let status = match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => resp.status(),
        other => panic!("expected HTTP rejection, got {other:?}"),
    };
    assert_eq!(status.as_u16(), 403, "cross-workspace yjs should be 403");
}
