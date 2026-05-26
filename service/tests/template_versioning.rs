//! Integration tests for template versioning and publishing.
//!
//! Requires docker-compose postgres and NATS to be running.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;
use mekhan_service::models::template::WorkflowGraph;
use mekhan_service::yjs::persistence::YjsPersistence;
use yrs::{Any, Map, Out, ReadTxn, StateVector, Transact, WriteTxn};

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

// ---------------------------------------------------------------------------
// Publish: create -> publish -> 200, published=true, air_json populated
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publish_template_sets_published_and_air() {
    let (app, _db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    // Create
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Publishable",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp.into_body()).await;
    let id = created["id"].as_str().unwrap();

    // Publish
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["published"], true);
    assert!(
        body["air_json"].is_object(),
        "published template should have air_json populated"
    );
    assert!(
        body["published_at"].is_string(),
        "published_at should be set"
    );
}

// ---------------------------------------------------------------------------
// Publish already-published -> 409
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publish_already_published_returns_409() {
    let (app, _db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    // Create and publish
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Already Published",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let created = body_json(resp.into_body()).await;
    let id = created["id"].as_str().unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Publish again -> 409
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// New version from published -> 201, version=2, is_latest=true,
// old version is_latest=false
// ---------------------------------------------------------------------------

#[tokio::test]
async fn new_version_from_published() {
    let (app, db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    // Create
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Versionable",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let created = body_json(resp.into_body()).await;
    let v1_id = created["id"].as_str().unwrap().to_string();

    // Publish v1
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{v1_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Create new version
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{v1_id}/new-version"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let v2 = body_json(resp.into_body()).await;
    assert_eq!(v2["version"], 2);
    assert_eq!(v2["is_latest"], true);
    assert_eq!(v2["published"], false);
    assert_eq!(v2["name"], "Versionable");

    // Verify old version is no longer latest
    let v1_id_uuid: Uuid = v1_id.parse().unwrap();
    let (is_latest,): (bool,) = sqlx::query_as(
        "SELECT is_latest FROM workflow_templates WHERE id = $1",
    )
    .bind(v1_id_uuid)
    .fetch_one(&db)
    .await
    .unwrap();

    assert!(!is_latest, "v1 should no longer be is_latest");
}

// ---------------------------------------------------------------------------
// New version from draft -> 409
// ---------------------------------------------------------------------------

#[tokio::test]
async fn new_version_from_draft_returns_409() {
    let (app, _db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    // Create (unpublished = draft)
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Draft Only",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let created = body_json(resp.into_body()).await;
    let id = created["id"].as_str().unwrap();

    // Try new-version from draft -> 409
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{id}/new-version"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// List versions -> returns all versions ordered by version DESC
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_versions_returns_ordered() {
    let (app, _db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    // Create
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Multi Version",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let v1 = body_json(resp.into_body()).await;
    let v1_id = v1["id"].as_str().unwrap().to_string();

    // Publish v1
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{v1_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Create v2
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{v1_id}/new-version"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v2 = body_json(resp.into_body()).await;
    let v2_id = v2["id"].as_str().unwrap().to_string();

    // Publish v2
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{v2_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Create v3
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{v2_id}/new-version"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // List versions (using any version's id)
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&format!("/api/v1/templates/{v1_id}/versions"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let versions: Vec<Value> = serde_json::from_slice(
        &resp.into_body().collect().await.unwrap().to_bytes(),
    )
    .unwrap();

    assert_eq!(versions.len(), 3, "should have 3 versions");

    // Ordered by version DESC
    let version_nums: Vec<i64> = versions
        .iter()
        .map(|v| v["version"].as_i64().unwrap())
        .collect();
    assert_eq!(version_nums, vec![3, 2, 1]);
}

// ---------------------------------------------------------------------------
// GET /api/v1/templates/:id/air -> AIR for published template
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_air_for_published_template() {
    let (app, _db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    // Create and publish
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "AIR Template",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let created = body_json(resp.into_body()).await;
    let id = created["id"].as_str().unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Get AIR
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&format!("/api/v1/templates/{id}/air"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let air = body_json(resp.into_body()).await;
    assert!(air.get("places").is_some());
    assert!(air.get("transitions").is_some());
}

// ---------------------------------------------------------------------------
// GET /api/v1/templates/:id/air -> 409 for unpublished template
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_air_for_unpublished_returns_409() {
    let (app, _db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Unpublished AIR",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let created = body_json(resp.into_body()).await;
    let id = created["id"].as_str().unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&format!("/api/v1/templates/{id}/air"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// Regression: new-version must fork the *source's authored Y.Doc*, not the
// stale/blank `graph` DB column. Canvas edits + node files live only in the
// Y.Doc (publish/edit never write the column back); before the fix,
// new-version reseeded from the empty column → blank canvas.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn new_version_forks_authored_ydoc_graph() {
    let (app, db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    // Create template (also seeds Y.Doc with the default graph incl. "start").
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": "Forkable", "author_id": author_id }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let created = body_json(resp.into_body()).await;
    let v1_id: Uuid = serde_json::from_value(created["id"].clone()).unwrap();

    // Author into the Y.Doc only (mirrors canvas editing — the `graph`
    // column is never touched by normal authoring).
    let persistence = YjsPersistence::new(db.clone());
    let doc = persistence.load_doc(v1_id).await.unwrap();
    let update = tokio::task::spawn_blocking(move || {
        {
            let mut txn = doc.transact_mut();
            let nodes_map = txn.get_or_insert_map("nodes");
            if let Some(Out::YMap(start_node)) = nodes_map.get(&txn, "start") {
                start_node.insert(&mut txn, "label", "Authored In YDoc");
            }
        }
        let txn = doc.transact();
        txn.encode_state_as_update_v1(&StateVector::default())
    })
    .await
    .unwrap();
    persistence.store_update(v1_id, &update).await.unwrap();

    // Publish v1 (reconstructs AIR from the Y.Doc but does not write the
    // `graph` column back).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{v1_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Fork a new version.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{v1_id}/new-version"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v2 = body_json(resp.into_body()).await;
    let v2_id: Uuid = serde_json::from_value(v2["id"].clone()).unwrap();

    // The forked version's Y.Doc must carry the authored label — not a
    // blank canvas reseeded from the empty `graph` column.
    let doc2 = persistence.load_doc(v2_id).await.unwrap();
    let label = tokio::task::spawn_blocking(move || {
        let mut txn = doc2.transact_mut();
        let nodes_map = txn.get_or_insert_map("nodes");
        match nodes_map.get(&txn, "start") {
            Some(Out::YMap(start_node)) => match start_node.get(&txn, "label") {
                Some(Out::Any(Any::String(s))) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    })
    .await
    .unwrap();

    assert_eq!(
        label.as_deref(),
        Some("Authored In YDoc"),
        "new version must fork the source's authored Y.Doc graph, not a blank canvas"
    );
}

// ---------------------------------------------------------------------------
// GitOps `apply`: create draft -> apply -> seeded+published v1 w/ provenance
// ---------------------------------------------------------------------------

async fn create_draft(app: &axum::Router, name: &str) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "name": name, "author_id": Uuid::new_v4() }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let created = body_json(resp.into_body()).await;
    created["id"].as_str().unwrap().to_string()
}

fn apply_body() -> String {
    json!({
        "graph": WorkflowGraph::default_graph(),
        "source_ref": { "remote": "git@forge:wf.git", "sha": "deadbeef", "dirty": false, "ref": "main" }
    })
    .to_string()
}

async fn apply(app: &axum::Router, id: &str) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{id}/apply"))
                .header("content-type", "application/json")
                .body(Body::from(apply_body()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn apply_seeds_fresh_init_draft_as_v1() {
    let (app, db) = common::test_app().await;
    let id = create_draft(&app, "GitOps Seed").await;

    let resp = apply(&app, &id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp.into_body()).await;
    assert_eq!(v["version"], 1);
    assert_eq!(v["published"], true);
    assert_eq!(v["is_latest"], true);

    // Seed publishes the same v1 row in place (no bump).
    let id_uuid: Uuid = id.parse().unwrap();
    let (source_ref,): (Option<Value>,) =
        sqlx::query_as("SELECT source_ref FROM workflow_templates WHERE id = $1")
            .bind(id_uuid)
            .fetch_one(&db)
            .await
            .unwrap();
    let sr = source_ref.expect("source_ref must be stamped");
    assert_eq!(sr["sha"], "deadbeef");
    assert_eq!(sr["ref"], "main");
}

#[tokio::test]
async fn apply_bumps_published_head() {
    let (app, db) = common::test_app().await;
    let v1_id = create_draft(&app, "GitOps Bump").await;

    // Publish v1 the normal way (UI publish leaves source_ref NULL).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{v1_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Apply -> born-published v2.
    let resp = apply(&app, &v1_id).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let v2 = body_json(resp.into_body()).await;
    assert_eq!(v2["version"], 2);
    assert_eq!(v2["published"], true);
    assert_eq!(v2["is_latest"], true);
    assert_eq!(v2["source_ref"]["sha"], "deadbeef");

    let v1_uuid: Uuid = v1_id.parse().unwrap();
    let (is_latest, sr): (bool, Option<Value>) = sqlx::query_as(
        "SELECT is_latest, source_ref FROM workflow_templates WHERE id = $1",
    )
    .bind(v1_uuid)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(!is_latest, "v1 must no longer be latest");
    assert!(sr.is_none(), "UI-published v1 must keep source_ref NULL");
}

#[tokio::test]
async fn apply_rejects_ui_new_version_draft() {
    let (app, _db) = common::test_app().await;
    let v1_id = create_draft(&app, "GitOps Conflict").await;

    // Publish v1, then open a UI new-version draft (v2, unpublished).
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{v1_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&format!("/api/v1/templates/{v1_id}/new-version"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Apply against the chain must 409 — the head is a web-editor draft.
    let resp = apply(&app, &v1_id).await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}
