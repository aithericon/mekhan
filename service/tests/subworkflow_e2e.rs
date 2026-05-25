//! End-to-end integration tests for the SubWorkflow call/return keystone.
//!
//! Covers the publish-time pin-and-freeze guarantee (deterministic, no engine)
//! and the real spawn/reply round-trip against the live engine.
//!
//! Requires Postgres + NATS (both tests) and, for the engine-backed test, the
//! full `just dev up` stack (engine on :3030 sharing the dev NATS broker).
//! Run the engine-backed test serially with `--test-threads=1` (shared engine).

mod common;

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::models::template::{
    default_subworkflow_output_port, Port, Position, VersionPin, WorkflowEdge,
    WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use mekhan_service::nats::MekhanNats;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn pos() -> Position {
    Position { x: 0.0, y: 0.0 }
}

fn start(id: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "start".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::Start {
            label: "Start".to_string(),
            description: None,
            initial: Port::empty_input(),
            process_name: None,
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn end(id: &str) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "end".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::End {
            label: "End".to_string(),
            description: None,
            terminal: mekhan_service::models::template::default_terminal_port(),
            result_mapping: Vec::new(),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn subworkflow(id: &str, child_family: Uuid, pin: VersionPin) -> WorkflowNode {
    WorkflowNode {
        id: id.to_string(),
        node_type: "sub_workflow".to_string(),
        slug: None,
        position: pos(),
        data: WorkflowNodeData::SubWorkflow {
            label: "Call Child".to_string(),
            description: None,
            template_id: child_family,
            version_pin: pin,
            input_mapping: Vec::new(),
            output: default_subworkflow_output_port(),
        },
        parent_id: None,
        width: None,
        height: None,
    }
}

fn edge(id: &str, source: &str, target: &str) -> WorkflowEdge {
    WorkflowEdge {
        id: id.to_string(),
        source: source.to_string(),
        target: target.to_string(),
        source_handle: None,
        target_handle: Some("in".to_string()),
        label: None,
        edge_type: "sequence".to_string(),
    }
}

/// `Start → End` child workflow. The version `tag` is baked into the node
/// ids, so the compiler emits it into place/transition ids (e.g.
/// `p_<tag>start_ready`) — a fingerprint that survives compilation and
/// `make_child_callable` into the embedded scenario, unlike a display label.
fn child_graph(tag: &str) -> WorkflowGraph {
    let s = format!("{tag}start");
    let e = format!("{tag}end");
    WorkflowGraph {
        nodes: vec![start(&s), end(&e)],
        edges: vec![edge("ce", &s, &e)],
        viewport: None, instance_concurrency: Default::default(),
    }
}

/// `Start → SubWorkflow → End` — the parent workflow.
fn parent_graph(child_family: Uuid, pin: VersionPin) -> WorkflowGraph {
    WorkflowGraph {
        nodes: vec![
            start("pstart"),
            subworkflow("sub", child_family, pin),
            end("pend"),
        ],
        edges: vec![
            edge("pe1", "pstart", "sub"),
            edge("pe2", "sub", "pend"),
        ],
        viewport: None, instance_concurrency: Default::default(),
    }
}

async fn create_with_graph(
    app: &axum::Router,
    name: &str,
    graph: &WorkflowGraph,
) -> Uuid {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": name,
                        "graph": graph,
                        "author_id": Uuid::new_v4(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create {name}");
    let created = body_json(resp.into_body()).await;
    created["id"].as_str().unwrap().parse().unwrap()
}

async fn publish(app: &axum::Router, id: Uuid) -> Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/templates/{id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "publish {id}: {body}");
    body
}

// ---------------------------------------------------------------------------
// 1. Pin-at-publish freeze (deterministic — no engine)
// ---------------------------------------------------------------------------

/// The keystone guarantee: a parent that references a child resolves and
/// **freezes** the concrete child AIR at the parent's publish time. Mutating
/// and republishing the child must NOT change the already-published parent;
/// a `Latest` pin re-resolves only on the *next* parent publish.
#[tokio::test]
async fn subworkflow_pins_child_at_parent_publish() {
    let (app, db) = common::test_app().await;

    // Child v1: Start → End, node ids fingerprinted "cv1", published.
    let child_v1 = create_with_graph(&app, "Child", &child_graph("cv1")).await;
    let cv1 = publish(&app, child_v1).await;
    assert!(cv1["air_json"].is_object(), "child v1 air");

    // Parent pins child v1 explicitly.
    let parent =
        create_with_graph(&app, "Parent", &parent_graph(child_v1, VersionPin::Pinned { version: 1 }))
            .await;
    let pub_body = publish(&app, parent).await;
    let parent_air_v1 = pub_body["air_json"].clone();
    assert!(parent_air_v1.is_object(), "parent air populated");

    // The resolved child was embedded + made spawn-callable: the parent AIR
    // carries the spawn transition and the embedded child scenario exposes the
    // fixed callable boundary (inbox / reply_out / fail_out) plus child v1's
    // fingerprint.
    let air_str = serde_json::to_string(&parent_air_v1).unwrap();
    assert!(air_str.contains("t_sub_spawn"), "expected spawn transition");
    for marker in ["\"inbox\"", "reply_out", "fail_out", "cv1start"] {
        assert!(
            air_str.contains(marker),
            "parent AIR must embed callable child ({marker} missing)"
        );
    }
    assert!(
        !air_str.contains("cv2start"),
        "parent AIR must not contain a not-yet-existent child version"
    );

    // Mutate the child: new version v2 with a materially different graph.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/templates/{child_v1}/new-version"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "child new-version");
    let v2 = body_json(resp.into_body()).await;
    let child_v2: Uuid = v2["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(v2["version"], 2);

    // Replace v2's graph via the DB column and drop its Y.Doc so publish uses
    // the DB-graph fallback (mirrors yjs_publish_flow::publish_falls_back_to_db_graph).
    let v2_graph = serde_json::to_value(child_graph("cv2")).unwrap();
    sqlx::query("UPDATE workflow_templates SET graph = $1 WHERE id = $2")
        .bind(&v2_graph)
        .bind(child_v2)
        .execute(&db)
        .await
        .unwrap();
    for tbl in ["yjs_documents", "yjs_snapshots"] {
        sqlx::query(&format!("DELETE FROM {tbl} WHERE template_id = $1"))
            .bind(child_v2)
            .execute(&db)
            .await
            .unwrap();
    }
    let cv2 = publish(&app, child_v2).await;
    assert!(
        serde_json::to_string(&cv2["air_json"]).unwrap().contains("cv2start"),
        "child v2 should compile its own new graph"
    );

    // FREEZE: the already-published parent's stored AIR is returned verbatim
    // by GET /air and must be byte-identical despite the child changing.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/templates/{parent}/air"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "get parent air");
    let parent_air_after = body_json(resp.into_body()).await;
    assert_eq!(
        parent_air_after, parent_air_v1,
        "pin-at-publish: a published parent's AIR must not change when the child changes"
    );

    // A NEW parent on `Latest`, published *after* child v2, re-resolves to v2
    // — proving the freeze is specific to already-published parents, not the
    // resolver ignoring newer versions.
    let parent2 =
        create_with_graph(&app, "Parent Latest", &parent_graph(child_v1, VersionPin::Latest))
            .await;
    let p2 = publish(&app, parent2).await;
    let p2_air = serde_json::to_string(&p2["air_json"]).unwrap();
    assert!(
        p2_air.contains("cv2start") && !p2_air.contains("cv1start"),
        "a Latest-pinned parent published after v2 must embed v2, not v1"
    );
}

// ---------------------------------------------------------------------------
// 2. Real spawn / reply / completion against the live engine
// ---------------------------------------------------------------------------

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:3030".to_string())
}

async fn engine_available() -> bool {
    matches!(
        reqwest::get(format!("{}/api/nets/metadata", engine_url())).await,
        Ok(resp) if resp.status().is_success()
    )
}

/// Parent instance spawns the pinned child, the child replies then quiesces,
/// the parent joins the reply and runs through to its End → `completed`.
#[tokio::test]
async fn subworkflow_spawns_child_and_completes() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }

    let engine_nats_url =
        std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) =
        common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

    // Lifecycle listener on the engine's NATS so instance status reaches the DB.
    let listener_nats = MekhanNats::connect(&engine_nats_url, None).await.expect("nats");
    let kv = listener_nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("kv");
    let sub_mgr = std::sync::Arc::new(SubscriptionManager::new(
        kv,
        listener_nats.jetstream().clone(),
    ));
    let listener_db = db.clone();
    tokio::spawn(async move {
        start_lifecycle_listener(
            listener_nats,
            listener_db,
            sub_mgr,
            None,
            mekhan_service::triggers::ResultWaiters::new(),
        )
        .await;
    });

    // Child + parent, both published.
    let child = create_with_graph(&app, "E2E Child", &child_graph("e2ec")).await;
    publish(&app, child).await;
    let parent = create_with_graph(
        &app,
        "E2E Parent",
        &parent_graph(child, VersionPin::Latest),
    )
    .await;
    publish(&app, parent).await;

    // Create an instance of the parent (deploys + sets running).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "template_id": parent,
                        "created_by": Uuid::new_v4(),
                        "metadata": { "e2e": "subworkflow" }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let inst_status = resp.status();
    let instance = body_json(resp.into_body()).await;
    assert_eq!(
        inst_status,
        StatusCode::CREATED,
        "create instance: {instance}"
    );
    let instance_id: Uuid = instance["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(instance["status"], "running");

    // The parent must spawn the child, correlate its reply, and run to End.
    let deadline = Duration::from_secs(30);
    let started = std::time::Instant::now();
    loop {
        let status: String =
            sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
                .bind(instance_id)
                .fetch_one(&db)
                .await
                .unwrap();
        if status == "completed" {
            break;
        }
        assert_ne!(status, "failed", "parent instance failed");
        if started.elapsed() > deadline {
            panic!("parent did not complete within {deadline:?} (status: {status})");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

// ---------------------------------------------------------------------------
// 3. SubWorkflow as a parked producer — `<sub_slug>.<field>` borrow round-trip
// ---------------------------------------------------------------------------

/// Like `create_with_graph`, but takes an already-shaped JSON graph so the
/// child + parent can declare custom Start input fields, End result mappings,
/// and a slug on the SubWorkflow node without rebuilding the typed helpers.
async fn create_with_graph_json(
    app: &axum::Router,
    name: &str,
    graph: &Value,
) -> Uuid {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": name,
                        "graph": graph,
                        "author_id": Uuid::new_v4(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create {name}");
    let created = body_json(resp.into_body()).await;
    created["id"].as_str().unwrap().parse().unwrap()
}

/// SubWorkflow becomes a parked producer: `<sub_slug>.<field>` resolves
/// downstream via the same read-arc pipeline as HumanTask/AutomatedStep.
///
/// The compile-time shape is asserted by
/// `subworkflow_slug_borrow_and_join_unwraps_exit_code` in
/// `service/src/compiler/compile.rs` (read-arc on `p_sub_data`, `sub.greeting`
/// rewritten to `d_sub.greeting`, join unwraps `exit_code.value`). This test
/// closes the runtime side end-to-end:
///
///   - child End stamps `exit_code.value.greeting` on the reply token
///   - parent `t_sub_join` unwraps that envelope and parks `{greeting}` into
///     `p_sub_data` via `split_outputs`
///   - parent End read-arcs `p_sub_data`, projects `d_sub.greeting` into the
///     success envelope (`result.value.greeting`)
///
/// Without any one of those steps the assertion fails — the borrow either
/// returns null or vanishes through the executor envelope.
#[tokio::test]
async fn subworkflow_borrows_child_output_field() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }

    let engine_nats_url =
        std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) =
        common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

    // Lifecycle listener — without this the instance status never advances
    // past `running` in Postgres even though the engine completes the net.
    let listener_nats = MekhanNats::connect(&engine_nats_url, None).await.expect("nats");
    let kv = listener_nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("kv");
    let sub_mgr = std::sync::Arc::new(SubscriptionManager::new(
        kv,
        listener_nats.jetstream().clone(),
    ));
    let listener_db = db.clone();
    tokio::spawn(async move {
        start_lifecycle_listener(
            listener_nats,
            listener_db,
            sub_mgr,
            None,
            mekhan_service::triggers::ResultWaiters::new(),
        )
        .await;
    });

    // Child: Start(name: text) → End(greeting = "Hello, " + input.name)
    // The End's `resultMapping` is what the child's terminal reply carries,
    // nested at `exit_code.value.greeting` per lower_end's result_shape.
    let child = json!({
        "nodes": [
            { "id": "cstart", "type": "start", "position": { "x": 0, "y": 0 },
              "data": {
                  "type": "start", "label": "Child Start",
                  "initial": {
                      "id": "in", "label": "Input",
                      "fields": [
                          { "name": "name", "label": "Name",
                            "kind": "text", "required": true }
                      ]
                  }
              } },
            { "id": "cend", "type": "end", "position": { "x": 240, "y": 0 },
              "data": {
                  "type": "end", "label": "Child End",
                  "resultMapping": [
                      { "targetField": "greeting",
                        "expression": "\"Hello, \" + input.name" }
                  ]
              } }
        ],
        "edges": [
            { "id": "ce", "source": "cstart", "target": "cend",
              "targetHandle": "in", "type": "sequence" }
        ]
    });
    let child_id = create_with_graph_json(&app, "Borrow Child", &child).await;
    publish(&app, child_id).await;

    // Parent: Start(name) → SubWorkflow(slug=sub, output.greeting) → End(greeting = sub.greeting)
    //
    // - `slug: "sub"` makes `sub.<field>` references in downstream Rhai
    //   resolvable by the read-arc synthesis pipeline.
    // - SubWorkflow's `output.fields = [greeting]` switches `lower_subworkflow`'s
    //   `join_logic` from opaque pass-through to declared-field projection;
    //   the joined token at `p_<sub>_output` becomes `{output: {greeting: ...}}`.
    //   `split_outputs` then parks `{greeting}` into `p_sub_data`.
    // - End's mapping `greeting = sub.greeting` triggers `apply_control_data_foundation`'s
    //   read-arc synthesis: rewrites to `d_sub.greeting`, takes a read-arc on
    //   `p_sub_data` (port `d_sub`).
    let parent = json!({
        "nodes": [
            { "id": "pstart", "type": "start", "position": { "x": 0, "y": 0 },
              "data": {
                  "type": "start", "label": "Parent Start",
                  "initial": {
                      "id": "in", "label": "Input",
                      "fields": [
                          { "name": "name", "label": "Name",
                            "kind": "text", "required": true }
                      ]
                  }
              } },
            { "id": "sub", "type": "sub_workflow", "slug": "sub",
              "position": { "x": 240, "y": 0 },
              "data": {
                  "type": "sub_workflow", "label": "Call Child",
                  "templateId": child_id,
                  "versionPin": { "mode": "latest" },
                  "inputMapping": [],
                  "output": {
                      "id": "out", "label": "Out",
                      "fields": [
                          { "name": "greeting", "label": "Greeting",
                            "kind": "text", "required": true }
                      ]
                  }
              } },
            { "id": "pend", "type": "end", "position": { "x": 480, "y": 0 },
              "data": {
                  "type": "end", "label": "Parent End",
                  "resultMapping": [
                      { "targetField": "greeting",
                        "expression": "sub.greeting" }
                  ]
              } }
        ],
        "edges": [
            { "id": "pe1", "source": "pstart", "target": "sub",
              "targetHandle": "in", "type": "sequence" },
            { "id": "pe2", "source": "sub", "target": "pend",
              "targetHandle": "in", "type": "sequence" }
        ]
    });
    let parent_id = create_with_graph_json(&app, "Borrow Parent", &parent).await;
    publish(&app, parent_id).await;

    // Instantiate with `{name: "world"}` — the child's End computes
    // `"Hello, world"`, the parent's End should surface it via `sub.greeting`.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "template_id": parent_id,
                        "created_by": Uuid::new_v4(),
                        "metadata": { "e2e": "subworkflow_borrow" },
                        "start_tokens": [{
                            "start_block_id": "pstart",
                            "token": { "name": "world" },
                        }],
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let inst_status = resp.status();
    let instance = body_json(resp.into_body()).await;
    assert_eq!(
        inst_status,
        StatusCode::CREATED,
        "create instance: {instance}"
    );
    let instance_id: Uuid = instance["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(instance["status"], "running");

    let deadline = Duration::from_secs(30);
    let started = std::time::Instant::now();
    loop {
        let status: String =
            sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
                .bind(instance_id)
                .fetch_one(&db)
                .await
                .unwrap();
        if status == "completed" {
            break;
        }
        if status == "failed" {
            let result: Option<Value> = sqlx::query_scalar(
                "SELECT result FROM workflow_instances WHERE id = $1",
            )
            .bind(instance_id)
            .fetch_one(&db)
            .await
            .unwrap();
            panic!("parent instance failed (result: {result:?})");
        }
        if started.elapsed() > deadline {
            panic!("parent did not complete within {deadline:?} (status: {status})");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let result: Value =
        sqlx::query_scalar::<_, Option<Value>>(
            "SELECT result FROM workflow_instances WHERE id = $1",
        )
        .bind(instance_id)
        .fetch_one(&db)
        .await
        .unwrap()
        .expect("result column was null — End's resultMapping produced no envelope");

    assert_eq!(
        result["ok"], json!(true),
        "expected success envelope on parent, got: {result}"
    );
    assert_eq!(
        result["value"]["greeting"], json!("Hello, world"),
        "parent End should borrow `sub.greeting` from the child via read-arc \
         on p_sub_data. Got: {result}. If null/missing, the parked envelope \
         isn't reaching the End — check t_sub_join's exit_code.value unwrap \
         and split_outputs' parking shape."
    );
}
