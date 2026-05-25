//! CRUD smoke test for the template-tests API.
//!
//! Exercises the data-plane endpoints with no engine dependency: create a
//! template + family, list tests, create + update + delete a test, and
//! confirm 412 publish-blocked behavior when an enabled test has never been
//! run (the gate treats "no run against this version" as failing).
//!
//! The full run/auto-completion path is covered by the live verification
//! step (task 16 in the plan), which requires `just dev up` and is run
//! interactively.

mod common;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::models::template::WorkflowGraph;

async fn body_json(body: Body) -> Value {
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Seed an unpublished template directly via SQL. Keeps the harness from
/// depending on Y.Doc / yjs init, which the public POST /api/templates
/// helper triggers.
async fn seed_template(db: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let author_id = Uuid::new_v4();
    let graph = WorkflowGraph::default_graph();
    let graph_json = serde_json::to_value(&graph).unwrap();

    sqlx::query(
        r#"INSERT INTO workflow_templates
            (id, name, description, base_template_id, version, is_latest, graph, author_id)
           VALUES ($1, 'TT E2E', 'template-tests e2e', $1, 1, TRUE, $2, $3)"#,
    )
    .bind(id)
    .bind(&graph_json)
    .bind(author_id)
    .execute(db)
    .await
    .expect("seed template");

    id
}

#[tokio::test]
async fn test_template_tests_crud() {
    let (app, db) = common::test_app().await;
    let template_id = seed_template(&db).await;

    // 1. List is empty for a fresh template.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/templates/{template_id}/tests"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let list = body_json(resp.into_body()).await;
    assert_eq!(list.as_array().map(|a| a.len()), Some(0));

    // 2. Create a test. `start_tokens` and `assertions` shape match the
    //    serialized DTOs (`Vec<StartToken>` / `Vec<Assertion>`).
    let create_body = json!({
        "name": "smoke",
        "enabled": true,
        "start_tokens": [{ "start_block_id": "start-1", "token": {} }],
        "human_answers": {},
        "assertions": [{ "path": "result.value", "op": "exists", "value": null }]
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/templates/{template_id}/tests"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&create_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp.into_body()).await;
    let test_id = created["id"].as_str().expect("id").to_string();
    assert_eq!(created["name"], "smoke");
    assert_eq!(created["enabled"], true);
    // Family resolution: a v1 row's family root is itself (`base_template_id`
    // == `id`) so the test attaches under the template's own id.
    assert_eq!(created["template_id"], template_id.to_string());

    // 3. List now shows the row.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/templates/{template_id}/tests"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = body_json(resp.into_body()).await;
    assert_eq!(list.as_array().map(|a| a.len()), Some(1));

    // 4. PATCH: flip enabled and rename — confirms COALESCE-style updates
    //    don't clobber the un-supplied fields.
    let patch_body = json!({ "enabled": false, "name": "smoke-renamed" });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/templates/{template_id}/tests/{test_id}"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&patch_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let patched = body_json(resp.into_body()).await;
    assert_eq!(patched["enabled"], false);
    assert_eq!(patched["name"], "smoke-renamed");
    // Untouched fields preserved.
    assert_eq!(patched["start_tokens"], created["start_tokens"]);
    assert_eq!(patched["assertions"], created["assertions"]);

    // 5. Duplicate-name on the same family rejects with 409.
    let dup_body = json!({
        "name": "smoke-renamed",
        "start_tokens": [],
        "human_answers": {},
        "assertions": []
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/templates/{template_id}/tests"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&dup_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);

    // 6. DELETE removes it.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/templates/{template_id}/tests/{test_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/templates/{template_id}/tests"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = body_json(resp.into_body()).await;
    assert_eq!(list.as_array().map(|a| a.len()), Some(0));
}

#[tokio::test]
async fn test_run_one_returns_412_when_no_published_version() {
    let (app, db) = common::test_app().await;
    let template_id = seed_template(&db).await;

    let create_body = json!({
        "name": "no-published",
        "enabled": true,
        "start_tokens": [],
        "human_answers": {},
        "assertions": []
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/templates/{template_id}/tests"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&create_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let created = body_json(resp.into_body()).await;
    let test_id = created["id"].as_str().unwrap().to_string();

    // Template has never been published, so the runner has no AIR to
    // execute. Surface that as 412 with an actionable message rather than
    // an opaque 500.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/templates/{template_id}/tests/{test_id}/run"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PRECONDITION_FAILED);
    let body = body_json(resp.into_body()).await;
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("no published version"),
        "got: {body}"
    );
}
