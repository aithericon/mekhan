//! Integration tests for template CRUD operations.
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
// POST /api/v1/templates -> 201, returns template with correct name
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_template_returns_201() {
    let (app, _db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Test Template",
                        "description": "A test",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = body_json(resp.into_body()).await;
    assert_eq!(body["name"], "Test Template");
    assert_eq!(body["description"], "A test");
    assert_eq!(body["version"], 1);
    assert_eq!(body["is_latest"], true);
    assert_eq!(body["published"], false);
    assert!(body["id"].is_string(), "should return an id");
}

// ---------------------------------------------------------------------------
// GET /api/v1/templates -> paginated list includes created template
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_templates_includes_created() {
    let (app, _db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    // Create a template
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Listed Template",
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
    let created_id = created["id"].as_str().unwrap();

    // List templates
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/templates")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;

    assert!(body["total"].as_i64().unwrap() >= 1);
    let items = body["items"].as_array().unwrap();
    assert!(
        items.iter().any(|t| t["id"].as_str() == Some(created_id)),
        "listed templates should include the created one"
    );
}

// ---------------------------------------------------------------------------
// GET /api/v1/templates/:id -> 200 with correct data
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_template_by_id_returns_200() {
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
                        "name": "Get Me",
                        "description": "should be fetchable",
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
                .uri(format!("/api/v1/templates/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["name"], "Get Me");
    assert_eq!(body["description"], "should be fetchable");
}

// ---------------------------------------------------------------------------
// GET /api/v1/templates/:nonexistent -> 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_nonexistent_template_returns_404() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/templates/{}", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// PUT /api/v1/templates/:id -> updates name/description
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_template_changes_name_and_description() {
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
                        "name": "Original Name",
                        "description": "original desc",
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
                .method("PUT")
                .uri(format!("/api/v1/templates/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Updated Name",
                        "description": "updated desc"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["name"], "Updated Name");
    assert_eq!(body["description"], "updated desc");
}

// ---------------------------------------------------------------------------
// PUT on published template -> 409
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_published_template_returns_409() {
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
                        "name": "Immutable Once Published",
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

    // Publish
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/templates/{id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Attempt update -> 409
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/v1/templates/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "Nope"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// DELETE /api/v1/templates/:id -> 204
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_template_returns_204() {
    let (app, db) = common::test_app().await;
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
                        "name": "To Delete",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let created = body_json(resp.into_body()).await;
    let id: Uuid = serde_json::from_value(created["id"].clone()).unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/templates/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Verify it's gone from DB
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM workflow_templates WHERE id = $1")
        .bind(id)
        .fetch_optional(&db)
        .await
        .unwrap();

    assert!(row.is_none(), "template should be deleted from DB");
}

// ---------------------------------------------------------------------------
// DELETE nonexistent -> 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_nonexistent_template_returns_404() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/templates/{}", Uuid::new_v4()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// POST /api/v1/templates/:id/compile -> preview compile (no publish)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compile_preview_returns_air_json() {
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
                        "name": "Compile Me",
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
                .method("POST")
                .uri(format!("/api/v1/templates/{id}/compile"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let air = body_json(resp.into_body()).await;
    assert!(air.get("places").is_some(), "AIR should have places");
    assert!(
        air.get("transitions").is_some(),
        "AIR should have transitions"
    );
}
