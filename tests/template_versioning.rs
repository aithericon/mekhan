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
                .uri("/api/templates")
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
                .uri(&format!("/api/templates/{id}/publish"))
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
                .uri("/api/templates")
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
                .uri(&format!("/api/templates/{id}/publish"))
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
                .uri(&format!("/api/templates/{id}/publish"))
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
                .uri("/api/templates")
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
                .uri(&format!("/api/templates/{v1_id}/publish"))
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
                .uri(&format!("/api/templates/{v1_id}/new-version"))
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
                .uri("/api/templates")
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
                .uri(&format!("/api/templates/{id}/new-version"))
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
                .uri("/api/templates")
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
                .uri(&format!("/api/templates/{v1_id}/publish"))
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
                .uri(&format!("/api/templates/{v1_id}/new-version"))
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
                .uri(&format!("/api/templates/{v2_id}/publish"))
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
                .uri(&format!("/api/templates/{v2_id}/new-version"))
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
                .uri(&format!("/api/templates/{v1_id}/versions"))
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
// GET /api/templates/:id/air -> AIR for published template
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
                .uri("/api/templates")
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
                .uri(&format!("/api/templates/{id}/publish"))
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
                .uri(&format!("/api/templates/{id}/air"))
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
// GET /api/templates/:id/air -> 409 for unpublished template
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
                .uri("/api/templates")
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
                .uri(&format!("/api/templates/{id}/air"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}
