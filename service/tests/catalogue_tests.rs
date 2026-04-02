//! Integration tests for the data catalogue endpoints.
//!
//! Tests the catalogue REST API with the new query infrastructure:
//! bracket-notation filters, sort, pagination, search, JSONB containment.
//!
//! Requires docker-compose postgres and NATS to be running:
//!   just -f aithericon-test-infra/justfile up

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Insert a catalogue entry directly into the database for testing.
async fn seed_entry(
    db: &sqlx::PgPool,
    id: &str,
    execution_id: &str,
    name: &str,
    category: &str,
    source_net: Option<&str>,
    process_id: Option<&str>,
    size_bytes: Option<i64>,
) {
    sqlx::query(
        r#"
        INSERT INTO catalogue_entries
            (id, execution_id, job_id, name, category, filename,
             source_net, process_id, size_bytes, file_metadata, user_metadata)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, '{}', '{}')
        "#,
    )
    .bind(id)
    .bind(execution_id)
    .bind(format!("{execution_id}:job"))
    .bind(name)
    .bind(category)
    .bind(format!("{name}.json"))
    .bind(source_net)
    .bind(process_id)
    .bind(size_bytes)
    .execute(db)
    .await
    .expect("seed catalogue entry");
}

// ---------------------------------------------------------------------------
// GET /api/catalogue -> empty list when no entries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_list_empty() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 0);
    assert!(body["items"].as_array().unwrap().is_empty());
    assert_eq!(body["page"], 0);
}

// ---------------------------------------------------------------------------
// GET /api/catalogue -> returns seeded entries (Paginated response)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_list_returns_seeded_entries() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "gp_model", "model", Some("bo-surrogate"), Some("campaign-1"), Some(1024)).await;
    seed_entry(&db, "art-2", "exec-2", "observations", "dataset", Some("bo-surrogate"), Some("campaign-1"), Some(512)).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 2);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["total_pages"], 1);
    assert_eq!(body["has_next"], false);
}

// ---------------------------------------------------------------------------
// Bracket-notation filter: filter[category][eq]=model
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_filter_bracket_notation() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "gp_model", "model", None, None, None).await;
    seed_entry(&db, "art-2", "exec-2", "data", "dataset", None, None, None).await;
    seed_entry(&db, "art-3", "exec-3", "chart", "plot", None, None, None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue?filter%5Bcategory%5D%5Beq%5D=model")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["category"], "model");
}

// ---------------------------------------------------------------------------
// Contains filter: filter[name][contains]=gp
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_filter_contains() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "gp_model_v1", "model", None, None, None).await;
    seed_entry(&db, "art-2", "exec-2", "gp_model_v2", "model", None, None, None).await;
    seed_entry(&db, "art-3", "exec-3", "observations", "dataset", None, None, None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue?filter%5Bname%5D%5Bcontains%5D=gp_model")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 2);
}

// ---------------------------------------------------------------------------
// IN filter: filter[category][in]=model,dataset
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_filter_in() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "model_a", "model", None, None, None).await;
    seed_entry(&db, "art-2", "exec-2", "data_a", "dataset", None, None, None).await;
    seed_entry(&db, "art-3", "exec-3", "chart_a", "plot", None, None, None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue?filter%5Bcategory%5D%5Bin%5D=model,dataset")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 2);
}

// ---------------------------------------------------------------------------
// Sort: sort=-size_bytes (descending)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_sort_desc() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "small", "model", None, None, Some(100)).await;
    seed_entry(&db, "art-2", "exec-2", "big", "model", None, None, Some(9999)).await;
    seed_entry(&db, "art-3", "exec-3", "medium", "model", None, None, Some(500)).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue?sort=-size_bytes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_json(resp.into_body()).await;
    let items = body["items"].as_array().unwrap();
    assert_eq!(items[0]["name"], "big");
    assert_eq!(items[1]["name"], "medium");
    assert_eq!(items[2]["name"], "small");
}

// ---------------------------------------------------------------------------
// Free-text search: search=gp
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_search() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "gp_model_rbf", "model", None, None, None).await;
    seed_entry(&db, "art-2", "exec-2", "observations", "dataset", None, None, None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue?search=gp_model")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["name"], "gp_model_rbf");
}

// ---------------------------------------------------------------------------
// Combined: filter + sort + search + pagination
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_combined_query() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "gp_model_v1", "model", Some("net-a"), None, Some(100)).await;
    seed_entry(&db, "art-2", "exec-2", "gp_model_v2", "model", Some("net-a"), None, Some(200)).await;
    seed_entry(&db, "art-3", "exec-3", "gp_model_v3", "model", Some("net-a"), None, Some(300)).await;
    seed_entry(&db, "art-4", "exec-4", "observations", "dataset", Some("net-a"), None, Some(50)).await;

    // Filter by category=model, search=gp, sort by size desc, page_size=2
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue?filter%5Bcategory%5D%5Beq%5D=model&search=gp&sort=-size_bytes&page_size=2&page=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 3); // 3 models match gp search
    assert_eq!(body["items"].as_array().unwrap().len(), 2); // page_size=2
    assert_eq!(body["items"][0]["name"], "gp_model_v3"); // largest first
    assert_eq!(body["items"][1]["name"], "gp_model_v2");
    assert_eq!(body["has_next"], true);
    assert_eq!(body["total_pages"], 2);
}

// ---------------------------------------------------------------------------
// Invalid filter field -> 400
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_invalid_filter_field() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue?filter%5Bsqlinjection%5D%5Beq%5D=bad")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp.into_body()).await;
    assert!(body["error"].as_str().unwrap().contains("invalid filter field"));
}

// ---------------------------------------------------------------------------
// Invalid sort field -> 400
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_invalid_sort_field() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue?sort=-not_a_column")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_json(resp.into_body()).await;
    assert!(body["error"].as_str().unwrap().contains("invalid sort field"));
}

// ---------------------------------------------------------------------------
// GET /api/catalogue/:exec/:id -> single entry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_get_single_entry() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-42", "my_model", "model", Some("test-net"), None, Some(2048)).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue/exec-42/art-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["id"], "art-1");
    assert_eq!(body["name"], "my_model");
}

// ---------------------------------------------------------------------------
// GET /api/catalogue/:exec/:id -> 404 for missing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_get_not_found() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue/nonexistent/nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// GET /api/catalogue/stats -> filterable statistics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_stats_filterable() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "model_1", "model", Some("net-a"), None, Some(1000)).await;
    seed_entry(&db, "art-2", "exec-2", "model_2", "model", Some("net-b"), None, Some(2000)).await;
    seed_entry(&db, "art-3", "exec-3", "data_1", "dataset", Some("net-a"), None, Some(500)).await;

    // Unfiltered stats
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/catalogue/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total_entries"], 3);
    assert_eq!(body["total_size_bytes"], 3500);

    // Filtered stats: only net-a
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue/stats?filter%5Bsource_net%5D%5Beq%5D=net-a")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total_entries"], 2);
    assert_eq!(body["total_size_bytes"], 1500);
}

// ---------------------------------------------------------------------------
// GET /api/catalogue/stats/by-net
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_stats_by_net() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "m1", "model", Some("net-a"), None, Some(100)).await;
    seed_entry(&db, "art-2", "exec-2", "m2", "model", Some("net-a"), None, Some(200)).await;
    seed_entry(&db, "art-3", "exec-3", "d1", "dataset", Some("net-b"), None, Some(50)).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue/stats/by-net")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    let stats = body.as_array().unwrap();
    assert_eq!(stats.len(), 2);
    assert_eq!(stats[0]["source_net"], "net-a");
    assert_eq!(stats[0]["total_artifacts"], 2);
}

// ---------------------------------------------------------------------------
// GET /api/catalogue/lineage/:process_id
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_lineage() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "iter_0_model", "model", None, Some("campaign-42"), None).await;
    seed_entry(&db, "art-2", "exec-2", "iter_1_model", "model", None, Some("campaign-42"), None).await;
    seed_entry(&db, "art-3", "exec-3", "other_model", "model", None, Some("campaign-99"), None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue/lineage/campaign-42")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let entries = body_json(resp.into_body()).await;
    let entries = entries.as_array().unwrap();
    assert_eq!(entries.len(), 2);
}

// ---------------------------------------------------------------------------
// GET /api/catalogue/distinct/category -> dropdown values
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_distinct_values() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "m1", "model", None, None, None).await;
    seed_entry(&db, "art-2", "exec-2", "d1", "dataset", None, None, None).await;
    seed_entry(&db, "art-3", "exec-3", "m2", "model", None, None, None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue/distinct/category")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let values = body_json(resp.into_body()).await;
    let values = values.as_array().unwrap();
    assert_eq!(values.len(), 2);
    assert!(values.contains(&Value::String("model".into())));
    assert!(values.contains(&Value::String("dataset".into())));
}

// ---------------------------------------------------------------------------
// JSONB metadata filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_filter_by_metadata() {
    let (app, db) = common::test_app().await;

    sqlx::query(
        r#"
        INSERT INTO catalogue_entries
            (id, execution_id, name, category, filename, file_metadata, user_metadata)
        VALUES
            ('art-1', 'exec-1', 'rbf_model', 'model', 'model.json', '{}', '{"kernel": "rbf", "lengthscale": "0.5"}'),
            ('art-2', 'exec-2', 'matern_model', 'model', 'model.json', '{}', '{"kernel": "matern"}')
        "#,
    )
    .execute(&db)
    .await
    .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue?metadata=%7B%22kernel%22%3A%22rbf%22%7D")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["name"], "rbf_model");
}

// ---------------------------------------------------------------------------
// Pagination: page=1, page_size=2
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalogue_pagination() {
    let (app, db) = common::test_app().await;

    seed_entry(&db, "art-1", "exec-1", "first", "model", None, None, None).await;
    seed_entry(&db, "art-2", "exec-2", "second", "model", None, None, None).await;
    seed_entry(&db, "art-3", "exec-3", "third", "model", None, None, None).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/catalogue?page=1&page_size=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 3);
    assert_eq!(body["items"].as_array().unwrap().len(), 1); // page 1 has 1 remaining
    assert_eq!(body["page"], 1);
    assert_eq!(body["page_size"], 2);
    assert_eq!(body["total_pages"], 2);
    assert_eq!(body["has_previous"], true);
    assert_eq!(body["has_next"], false);
}
