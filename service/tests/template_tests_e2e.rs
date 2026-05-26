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

use mekhan_service::models::template::{
    Port, PortField, Position, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};

async fn body_json(body: Body) -> Value {
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Seed an unpublished template directly via SQL. Keeps the harness from
/// depending on Y.Doc / yjs init, which the public POST /api/v1/templates
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
                .uri(format!("/api/v1/templates/{template_id}/tests"))
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
                .uri(format!("/api/v1/templates/{template_id}/tests"))
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
                .uri(format!("/api/v1/templates/{template_id}/tests"))
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
                .uri(format!("/api/v1/templates/{template_id}/tests/{test_id}"))
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
                .uri(format!("/api/v1/templates/{template_id}/tests"))
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
                .uri(format!("/api/v1/templates/{template_id}/tests/{test_id}"))
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
                .uri(format!("/api/v1/templates/{template_id}/tests"))
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
                .uri(format!("/api/v1/templates/{template_id}/tests"))
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
                    "/api/v1/templates/{template_id}/tests/{test_id}/run"
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

// --- Seeded extractor test ---------------------------------------------------
//
// Regression guard against the bug fixed in commit `506d973`: the promote
// handler used to query `causality_event_tokens.place_name = ... AND role =
// 'created'`, but ingest writes `place_id` (place_name is mostly NULL) and the
// engine emits a `produced` role — there is no `created`. The bug silently
// matched zero rows, so the test got promoted with an empty `start_tokens`
// and the runner blew up at launch with `MissingStartTokens(["start"])`.
//
// This test seeds the causality tables directly to avoid pulling in the
// engine: a Start-place `produced` token + a HumanTask signal-place `produced`
// token + an intentional decoy `consumed` row. After promote it asserts the
// resulting test row has both the start fixture and the human answer keyed by
// node slug, and that the decoy didn't bleed through.

/// Seed a template with a graph containing exactly one Start node and one
/// HumanTask node. Returns `(template_id, start_node_id, human_node_id,
/// human_slug)`. Slugs round-trip through `WorkflowNode::slug()` so the
/// asserted shape matches what the promote handler emits.
async fn seed_template_with_human_task(
    db: &sqlx::PgPool,
) -> (Uuid, String, String, String) {
    let template_id = Uuid::new_v4();
    let author_id = Uuid::new_v4();
    let start_id = "start".to_string();
    let human_id = "review".to_string();
    let human_slug = "review".to_string();

    let start_node = WorkflowNode {
        id: start_id.clone(),
        node_type: "start".to_string(),
        slug: None,
        position: Position { x: 0.0, y: 0.0 },
        data: WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial: Port {
                id: "initial".to_string(),
                label: "Input".to_string(),
                fields: vec![PortField {
                    name: "amount".to_string(),
                    label: "Amount".to_string(),
                    kind: mekhan_service::models::template::FieldKind::Number,
                    required: true,
                    options: None,
                    description: None,
                    accept: None,
                }],
            },
            process_name: None,
        },
        parent_id: None,
        width: None,
        height: None,
        tool_meta: None,
    };

    let human_node = WorkflowNode {
        id: human_id.clone(),
        node_type: "human_task".to_string(),
        slug: Some(human_slug.clone()),
        position: Position { x: 0.0, y: 0.0 },
        data: WorkflowNodeData::HumanTask {
            label: "Review".to_string(),
            description: None,
            task_title: "Review".to_string(),
            instructions_mdsvex: None,
            steps: vec![],
        },
        parent_id: None,
        width: None,
        height: None,
        tool_meta: None,
    };

    let graph = WorkflowGraph {
        nodes: vec![start_node, human_node],
        edges: Vec::<WorkflowEdge>::new(),
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
    };
    let graph_json = serde_json::to_value(&graph).unwrap();

    sqlx::query(
        r#"INSERT INTO workflow_templates
            (id, name, description, base_template_id, version, is_latest, graph, author_id)
           VALUES ($1, 'TT promote', 'promote-extractor seed', $1, 1, TRUE, $2, $3)"#,
    )
    .bind(template_id)
    .bind(&graph_json)
    .bind(author_id)
    .execute(db)
    .await
    .expect("seed template");

    (template_id, start_id, human_id, human_slug)
}

/// Drop a synthetic instance into `workflow_instances` and emit causality
/// rows that match the engine's actual shape (`place_id` set, `place_name`
/// NULL, role='produced'). The decoy 'consumed' / 'created' rows guard the
/// regression: the extractor must filter by both `place_id` and the right
/// role.
async fn seed_instance_with_events(
    db: &sqlx::PgPool,
    template_id: Uuid,
    start_id: &str,
    human_id: &str,
    start_token: &Value,
    human_completion: &Value,
) -> Uuid {
    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{instance_id}");
    let author_id = Uuid::new_v4();

    sqlx::query(
        r#"INSERT INTO workflow_instances
            (id, template_id, template_version, net_id, status, created_by, mode)
           VALUES ($1, $2, 1, $3, 'completed', $4, 'live')"#,
    )
    .bind(instance_id)
    .bind(template_id)
    .bind(&net_id)
    .bind(author_id)
    .execute(db)
    .await
    .expect("seed instance");

    // event_seq=1: start place gets the seed token (the real engine writes
    // this on net boot).
    sqlx::query(
        "INSERT INTO causality_events (net_id, event_seq, event_type, timestamp) \
         VALUES ($1, 1, 'token_produced', NOW())",
    )
    .bind(&net_id)
    .execute(db)
    .await
    .expect("seed start event");
    sqlx::query(
        "INSERT INTO causality_event_tokens \
            (net_id, event_seq, token_id, role, place_id, place_name, token_data) \
         VALUES ($1, 1, 'tok-start', 'produced', $2, NULL, $3)",
    )
    .bind(&net_id)
    .bind(format!("p_{start_id}_ready"))
    .bind(start_token)
    .execute(db)
    .await
    .expect("seed start token");

    // event_seq=2: signal place gets the human completion payload.
    sqlx::query(
        "INSERT INTO causality_events (net_id, event_seq, event_type, timestamp) \
         VALUES ($1, 2, 'token_produced', NOW())",
    )
    .bind(&net_id)
    .execute(db)
    .await
    .expect("seed signal event");
    sqlx::query(
        "INSERT INTO causality_event_tokens \
            (net_id, event_seq, token_id, role, place_id, place_name, token_data) \
         VALUES ($1, 2, 'tok-sig', 'produced', $2, NULL, $3)",
    )
    .bind(&net_id)
    .bind(format!("p_{human_id}_signal"))
    .bind(human_completion)
    .execute(db)
    .await
    .expect("seed signal token");

    // Decoys: a `consumed` row on the same start place (real engine emits one
    // when the Start's transition fires), and an `produced` row under the
    // legacy `place_name = 'p_<id>_ready'` shape we used to query by mistake.
    // Neither must contaminate the extraction.
    sqlx::query(
        "INSERT INTO causality_events (net_id, event_seq, event_type, timestamp) \
         VALUES ($1, 3, 'token_consumed', NOW())",
    )
    .bind(&net_id)
    .execute(db)
    .await
    .expect("seed decoy event");
    sqlx::query(
        "INSERT INTO causality_event_tokens \
            (net_id, event_seq, token_id, role, place_id, place_name, token_data) \
         VALUES ($1, 3, 'tok-start', 'consumed', $2, $3, NULL),
                ($1, 3, 'tok-decoy', 'produced', '', $3, $4)",
    )
    .bind(&net_id)
    .bind(format!("p_{start_id}_ready"))
    .bind(format!("p_{start_id}_ready"))
    .bind(json!({ "amount": 999 }))
    .execute(db)
    .await
    .expect("seed decoys");

    instance_id
}

#[tokio::test]
async fn test_promote_extracts_start_tokens_and_human_answers() {
    let (app, db) = common::test_app().await;

    let (template_id, start_id, human_id, human_slug) =
        seed_template_with_human_task(&db).await;

    let start_token = json!({ "amount": 1234, "_instance_id": "ignored" });
    let human_completion = json!({
        "task_id": "task-abc",
        "data": { "approved": true, "comment": "lgtm" },
        "completed_at": "2026-01-01T00:00:00Z",
    });
    let instance_id = seed_instance_with_events(
        &db,
        template_id,
        &start_id,
        &human_id,
        &start_token,
        &human_completion,
    )
    .await;

    // Promote the seeded instance.
    let body = json!({ "name": "promoted" });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/instances/{instance_id}/promote-to-test"))
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "promote should succeed");
    let test_row = body_json(resp.into_body()).await;

    // start_tokens should carry exactly the seeded token under
    // `start_block_id = <start_id>`, with the system field stripped.
    let start_tokens = test_row["start_tokens"]
        .as_array()
        .expect("start_tokens is an array");
    assert_eq!(start_tokens.len(), 1, "got: {test_row}");
    assert_eq!(start_tokens[0]["start_block_id"], start_id);
    let inner = &start_tokens[0]["token"];
    assert_eq!(inner["amount"], 1234, "got: {test_row}");
    assert!(
        inner.get("_instance_id").is_none(),
        "system fields must be stripped; got: {inner}"
    );

    // human_answers is keyed by the HumanTask's author slug. The engine wraps
    // signal payloads in `{ task_id, data, completed_at }`; the extractor
    // should unwrap to the inner `data` map.
    let answers = test_row["human_answers"]
        .as_object()
        .expect("human_answers is an object");
    let entry = answers
        .get(&human_slug)
        .unwrap_or_else(|| panic!("missing slug {human_slug:?} in {test_row}"));
    assert_eq!(entry["approved"], true);
    assert_eq!(entry["comment"], "lgtm");
    // Wrapper fields must not bleed through.
    assert!(entry.get("task_id").is_none(), "got: {entry}");
    assert!(entry.get("completed_at").is_none(), "got: {entry}");

    // reference_scope must be populated — that's the editor's "Available
    // scope" data. Empty scope is acceptable (no step_execution rows seeded);
    // the contract is just that the field is present and non-null.
    assert!(
        test_row["reference_scope"].is_object(),
        "expected reference_scope object, got: {}",
        test_row["reference_scope"]
    );
}
