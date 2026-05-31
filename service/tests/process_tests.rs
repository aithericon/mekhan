//! Integration tests for the process tracking endpoints.
//!
//! Tests the process REST API: list, get, update, metrics, logs, tasks, artifacts.
//!
//! Requires docker-compose postgres and NATS to be running:
//!   just -f aithericon-test-infra/justfile up

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Seed a process directly into the database.
async fn seed_process(
    db: &sqlx::PgPool,
    process_id: &str,
    name: Option<&str>,
    kind: Option<&str>,
    status: &str,
) {
    sqlx::query(
        r#"
        INSERT INTO hpi_processes (process_id, name, kind, status, config)
        VALUES ($1, $2, $3, $4, '{}')
        "#,
    )
    .bind(process_id)
    .bind(name)
    .bind(kind)
    .bind(status)
    .execute(db)
    .await
    .expect("seed process");
}

/// Seed a task attached to a process.
async fn seed_task(db: &sqlx::PgPool, id: &str, process_id: &str, title: &str, status: &str) {
    sqlx::query(
        r#"
        INSERT INTO hpi_tasks (id, process_id, title, status, detail)
        VALUES ($1, $2, $3, $4, '{}')
        "#,
    )
    .bind(id)
    .bind(process_id)
    .bind(title)
    .bind(status)
    .execute(db)
    .await
    .expect("seed task");
}

/// Seed a metric data point.
async fn seed_metric(db: &sqlx::PgPool, process_id: &str, key: &str, value: f64) {
    sqlx::query("INSERT INTO hpi_metrics (process_id, key, value) VALUES ($1, $2, $3)")
        .bind(process_id)
        .bind(key)
        .bind(value)
        .execute(db)
        .await
        .expect("seed metric");
}

/// Seed a log entry.
async fn seed_log(db: &sqlx::PgPool, process_id: &str, level: &str, source: &str, message: &str) {
    sqlx::query(
        "INSERT INTO hpi_logs (process_id, level, source, message, detail) VALUES ($1, $2, $3, $4, '{}')",
    )
    .bind(process_id)
    .bind(level)
    .bind(source)
    .bind(message)
    .execute(db)
    .await
    .expect("seed log");
}

/// Seed a catalogue entry with process_id for joining.
async fn seed_artifact(
    db: &sqlx::PgPool,
    id: &str,
    execution_id: &str,
    name: &str,
    process_id: &str,
) {
    sqlx::query(
        r#"
        INSERT INTO catalogue_entries
            (id, execution_id, job_id, name, category, filename, process_id, file_metadata, user_metadata)
        VALUES ($1, $2, $3, $4, 'model', $5, $6, '{}', '{}')
        "#,
    )
    .bind(id)
    .bind(execution_id)
    .bind(format!("{execution_id}:job"))
    .bind(name)
    .bind(format!("{name}.json"))
    .bind(process_id)
    .execute(db)
    .await
    .expect("seed catalogue entry with process_id");
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes -> empty list
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_list_empty() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/processes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 0);
    assert!(body["items"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes -> list with seeded data
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_list_with_data() {
    let (app, db) = common::test_app().await;

    seed_process(
        &db,
        "aaa111",
        Some("Campaign A"),
        Some("bo_campaign"),
        "active",
    )
    .await;
    seed_process(
        &db,
        "bbb222",
        Some("Campaign B"),
        Some("training"),
        "completed",
    )
    .await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/processes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 2);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes?filter[status][eq]=active
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_filter_by_status() {
    let (app, db) = common::test_app().await;

    seed_process(&db, "ccc333", Some("Active"), None, "active").await;
    seed_process(&db, "ddd444", Some("Done"), None, "completed").await;
    seed_process(&db, "eee555", Some("Failed"), None, "failed").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/processes?filter[status][eq]=active")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["name"], "Active");
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes?sort=-created_at
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_sort_desc() {
    let (app, db) = common::test_app().await;

    seed_process(&db, "fff666", Some("First"), None, "active").await;
    // Small delay to ensure ordering
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    seed_process(&db, "ggg777", Some("Second"), None, "active").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/processes?sort=-created_at")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["items"][0]["name"], "Second");
    assert_eq!(body["items"][1]["name"], "First");
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes?search=Campaign
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_search() {
    let (app, db) = common::test_app().await;

    seed_process(&db, "hhh888", Some("BO Campaign"), None, "active").await;
    seed_process(&db, "iii999", Some("Training Run"), None, "active").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/processes?search=Campaign")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["name"], "BO Campaign");
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes/{process_id} -> 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_get_not_found() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/processes/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes/{process_id} -> detail with tasks, metrics, logs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_get_detail() {
    let (app, db) = common::test_app().await;

    let tid = "detail-test-001";
    seed_process(&db, tid, Some("Detail Test"), Some("bo_campaign"), "active").await;
    seed_task(&db, "task-1", tid, "Review model", "pending").await;
    seed_metric(&db, tid, "best_f", 0.398).await;
    seed_metric(&db, tid, "sigma_avg", 0.12).await;
    seed_log(&db, tid, "info", "bo-oracle", "GP fit converged").await;
    seed_artifact(&db, "art-1", "exec-1", "gp_model", tid).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/processes/{tid}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["process_id"], tid);
    assert_eq!(body["name"], "Detail Test");
    assert_eq!(body["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(body["recent_metrics"].as_array().unwrap().len(), 2);
    assert_eq!(body["recent_logs"].as_array().unwrap().len(), 1);
    assert_eq!(body["artifact_count"], 1);
}

// ---------------------------------------------------------------------------
// PUT /api/v1/processes/{process_id} -> update name
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_update() {
    let (app, db) = common::test_app().await;

    let tid = "update-test-001";
    seed_process(&db, tid, None, None, "active").await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/v1/processes/{tid}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "name": "My Campaign",
                        "kind": "bo_campaign"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["name"], "My Campaign");
    assert_eq!(body["kind"], "bo_campaign");
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes/stats
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_stats() {
    let (app, db) = common::test_app().await;

    seed_process(&db, "stat-1", None, None, "active").await;
    seed_process(&db, "stat-2", None, None, "active").await;
    seed_process(&db, "stat-3", None, None, "completed").await;
    seed_process(&db, "stat-4", None, None, "failed").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/processes/stats")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 4);
    assert_eq!(body["active"], 2);
    assert_eq!(body["completed"], 1);
    assert_eq!(body["failed"], 1);
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes/{process_id}/metrics
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_metrics() {
    let (app, db) = common::test_app().await;

    let tid = "metric-test-001";
    seed_process(&db, tid, None, None, "active").await;
    seed_metric(&db, tid, "loss", 1.5).await;
    seed_metric(&db, tid, "loss", 1.2).await;
    seed_metric(&db, tid, "loss", 0.8).await;
    seed_metric(&db, tid, "accuracy", 0.95).await;

    // All metrics
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/processes/{tid}/metrics"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body.as_array().unwrap().len(), 4);

    // Filter by key
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/processes/{tid}/metrics?key=loss"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body.as_array().unwrap().len(), 3);
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes/{process_id}/logs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_logs() {
    let (app, db) = common::test_app().await;

    let tid = "log-test-001";
    seed_process(&db, tid, None, None, "active").await;
    seed_log(&db, tid, "info", "executor", "Job started").await;
    seed_log(&db, tid, "info", "executor", "Job completed").await;
    seed_log(&db, tid, "warn", "oracle", "High uncertainty").await;
    seed_log(&db, tid, "error", "executor", "OOM killed").await;

    // All logs
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/processes/{tid}/logs"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 4);

    // Filter by level
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/processes/{tid}/logs?filter[level][eq]=error"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["message"], "OOM killed");
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes/{process_id}/tasks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_tasks() {
    let (app, db) = common::test_app().await;

    let tid = "task-test-001";
    seed_process(&db, tid, None, None, "active").await;
    seed_task(&db, "t-1", tid, "Review checkpoint", "pending").await;
    seed_task(&db, "t-2", tid, "Approve report", "completed").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/processes/{tid}/tasks"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body.as_array().unwrap().len(), 2);
}

// ---------------------------------------------------------------------------
// GET /api/v1/processes/{process_id}/artifacts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_artifacts() {
    let (app, db) = common::test_app().await;

    let tid = "artifact-test-001";
    seed_process(&db, tid, None, None, "active").await;
    seed_artifact(&db, "a-1", "exec-1", "model_v0", tid).await;
    seed_artifact(&db, "a-2", "exec-2", "model_v1", tid).await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/processes/{tid}/artifacts"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 2);
}

// ---------------------------------------------------------------------------
// GET /api/v1/tasks -> list all tasks across processes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn task_list_all() {
    let (app, db) = common::test_app().await;

    seed_process(&db, "p-tasks-1", None, None, "active").await;
    seed_process(&db, "p-tasks-2", None, None, "active").await;
    seed_task(&db, "gt-1", "p-tasks-1", "Task A", "pending").await;
    seed_task(&db, "gt-2", "p-tasks-2", "Task B", "pending").await;
    seed_task(&db, "gt-3", "p-tasks-1", "Task C", "completed").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/tasks")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 3);
}

// ---------------------------------------------------------------------------
// GET /api/v1/tasks?filter[status][eq]=pending
// ---------------------------------------------------------------------------

#[tokio::test]
async fn task_filter_by_status() {
    let (app, db) = common::test_app().await;

    seed_process(&db, "p-tf-1", None, None, "active").await;
    seed_task(&db, "tf-1", "p-tf-1", "Pending Task", "pending").await;
    seed_task(&db, "tf-2", "p-tf-1", "Done Task", "completed").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/tasks?filter[status][eq]=pending")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["tasks"][0]["title"], "Pending Task");
}

// ---------------------------------------------------------------------------
// GET /api/v1/tasks/{id}
// ---------------------------------------------------------------------------

#[tokio::test]
async fn task_get_single() {
    let (app, db) = common::test_app().await;

    seed_process(&db, "p-tg-1", None, None, "active").await;
    seed_task(&db, "tg-1", "p-tg-1", "My Task", "pending").await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/tasks/tg-1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["id"], "tg-1");
    assert_eq!(body["title"], "My Task");
}

// ---------------------------------------------------------------------------
// GET /api/v1/tasks/{id} -> 404
// ---------------------------------------------------------------------------

#[tokio::test]
async fn task_get_not_found() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/tasks/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_pagination() {
    let (app, db) = common::test_app().await;

    for i in 0..5 {
        seed_process(
            &db,
            &format!("page-{i}"),
            Some(&format!("Process {i}")),
            None,
            "active",
        )
        .await;
    }

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/processes?page=0&page_size=2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["total"], 5);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["page"], 0);
    assert_eq!(body["page_size"], 2);
    assert_eq!(body["total_pages"], 3);
    assert_eq!(body["has_next"], true);
    assert_eq!(body["has_previous"], false);
}

// ---------------------------------------------------------------------------
// Invalid filter field -> 400
// ---------------------------------------------------------------------------

#[tokio::test]
async fn process_invalid_filter_field() {
    let (app, _db) = common::test_app().await;

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/processes?filter[nonexistent][eq]=foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
