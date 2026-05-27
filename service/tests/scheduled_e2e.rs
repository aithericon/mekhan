//! End-to-end coverage for a `Scheduled` AutomatedStep — the merged
//! deployment_model that dispatches through the long-lived `scheduler-net`
//! (real Nomad), the one keystone feature with no prior runtime proof.
//!
//! Unlike an Inline step (which the compiler lowers to a direct
//! executor-lifecycle), a Scheduled step compiles to a `bridge_out` carrying
//! a `SchedulerSubmitInput` to `scheduler-net/job_inbox` with `result` /
//! `failure` reply channels. The full path exercised here:
//!
//!   parent-net ─▶ scheduler-net ─(Nomad dispatch: petri-executor-worker)─▶
//!                 executor-net  ─▶ executor ─▶ result ─▶ back to parent-net
//!
//! This is the runtime counterpart to the static
//! `compiler_tests::automated_step_scheduled_emits_scheduler_bridge` unit
//! test — it proves the emitted bridge contract (net id, inbox place, token
//! shape, reply channels, per-job `job_template_id`) actually interoperates
//! with the real `scheduler-net`. It is the exact blind-spot class that
//! produced the `well_known.rs` scheduler-id bug.
//!
//! Requires the Nomad scheduler layer on top of `just dev up`:
//!
//!   just dev scheduler-up
//!
//! (Nomad agent :4646, petri-executor-worker registered, engine restarted
//! with SCHEDULER_BACKEND=nomad, scheduler-net + executor-net deployed &
//! running.) The Nomad-spawned executor pulls the staged main.py from the
//! dev rustfs bucket `mekhan-artifacts`, so this test needs the same S3
//! overrides as the other executor-backed e2e. Run serially
//! (`--test-threads=1`) — it shares the live engine/executor/Nomad.

mod common;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::models::template::{
    default_output_port, DeploymentModel, ExecutionBackendType, ExecutionSpecConfig, Port,
    Position, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use mekhan_service::nats::MekhanNats;

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

/// `Start → AutomatedStep(python, Scheduled via petri-executor-worker) → End`.
fn scheduled_graph(step_id: &str) -> WorkflowGraph {
    WorkflowGraph {
        nodes: vec![
            start("s"),
            WorkflowNode {
                id: step_id.to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::AutomatedStep {
                    label: "Run Python (Scheduled)".to_string(),
                    description: None,
                    execution_spec: ExecutionSpecConfig {
                        backend_type: ExecutionBackendType::Python,
                        entrypoint: Some("main.py".to_string()),
                        config: json!({
                            "python": "python3",
                            "requirements": [],
                            "virtualenv": false,
                            "sdk": true,
                            "inherit_env": true,
                            "env": {}
                        }),
                    },
                    input: Port::empty_input(),
                    output: default_output_port(ExecutionBackendType::Python),
                    retry_policy: Default::default(),
                    // The one thing under test: dispatch through scheduler-net
                    // (real Nomad), not the inline executor-lifecycle.
                    deployment_model: DeploymentModel::Scheduled {
                        job_template: "petri-executor-worker".to_string(),
                        resources: None,
                    },
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end("e"),
        ],
        edges: vec![
            WorkflowEdge {
                id: "e1".to_string(),
                source: "s".to_string(),
                target: step_id.to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
            WorkflowEdge {
                id: "e2".to_string(),
                source: step_id.to_string(),
                target: "e".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
        ],
        viewport: None, instance_concurrency: Default::default(), definitions: Default::default(),
    }
}

/// Minimal Aithericon-SDK Python step (same contract as the inline e2e): the
/// runner injects `set_output` / `log_info` / `token` as globals.
const MAIN_PY: &str = r#"log_info("scheduled automated-step e2e ran", task_id=token.get("task_id"))
set_output("ran", True)
set_output("answer", 42)
"#;

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:3030".to_string())
}

async fn engine_available() -> bool {
    matches!(
        reqwest::get(format!("{}/api/nets/metadata", engine_url())).await,
        Ok(resp) if resp.status().is_success()
    )
}

async fn net_running(net_id: &str) -> bool {
    match reqwest::get(format!("{}/api/nets/{net_id}/state", engine_url())).await {
        Ok(resp) if resp.status().is_success() => resp
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v.get("run_mode").and_then(|m| m.as_str()).map(str::to_string))
            .as_deref()
            == Some("running"),
        _ => false,
    }
}

#[tokio::test]
async fn scheduled_automated_step_runs_through_nomad() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }
    // Scheduled needs the Nomad layer; fail loud (not silent skip) so a
    // missing prerequisite is unambiguous.
    if !net_running("scheduler-net").await || !net_running("executor-net").await {
        panic!(
            "scheduler-net / executor-net not deployed+running — run `just dev scheduler-up`"
        );
    }

    let engine_nats_url =
        std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) =
        common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

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

    // Create with the inline script attached to the step node, then publish
    // (stages main.py + the generated _aithericon_io to rustfs S3).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Scheduled AutomatedStep E2E",
                        "graph": scheduled_graph("auto"),
                        "files": { "auto": { "main.py": MAIN_PY } },
                        "author_id": Uuid::new_v4(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create template");
    let created = body_json(resp.into_body()).await;
    let template_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/templates/{template_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let pub_body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "publish: {pub_body}");

    // Capture a Nomad SubmitTime lower bound BEFORE instance creation so the
    // post-completion Nomad assertion can prove that THIS test's dispatched
    // child (not a stale one from a prior run) actually processed a job.
    let submit_after_nanos: i64 = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;

    // Create an instance — deploys + Running. The Strict bridge gate here is
    // what the well_known.rs fix had to satisfy: the parent's bridge_out must
    // target a *deployed* scheduler-net/job_inbox or this 422s.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "template_id": template_id,
                        "created_by": Uuid::new_v4(),
                        "metadata": { "e2e": "scheduled_automated_step" }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let inst_status = resp.status();
    let instance = body_json(resp.into_body()).await;
    assert_eq!(inst_status, StatusCode::CREATED, "create instance: {instance}");
    let instance_id: Uuid = instance["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(instance["status"], "running");

    // scheduler-net submits a Nomad job (petri-executor-worker), which spawns
    // the executor; it pulls main.py from S3, runs python3, the result relays
    // back through executor-net → scheduler-net → the parent's reply channel,
    // and the parent net runs to End. Nomad dispatch + cold executor is
    // slower than the inline path, so allow a generous deadline.
    let deadline = Duration::from_secs(180);
    let started = std::time::Instant::now();
    loop {
        let st: String =
            sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
                .bind(instance_id)
                .fetch_one(&db)
                .await
                .unwrap();
        if st == "completed" {
            break;
        }
        assert_ne!(
            st, "failed",
            "instance failed — scheduler-net/Nomad/executor path did not succeed"
        );
        if started.elapsed() > deadline {
            panic!("instance did not complete within {deadline:?} (status: {st})");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Regression guard against the write_node_config bug: "instance
    // completes" is *also* true if the Scheduled step silently collapsed to
    // Inline (the executor-lifecycle still runs and reaches End). So assert
    // the deployed instance net actually carries the Scheduled lowering — the
    // `p_<step>_sched_out` bridge_out to scheduler-net — and NOT the inline
    // executor-lifecycle places (`<step>/submitted`, `<step>/inbox`). If
    // deployment_model is ever dropped on the graph→Y.Doc→publish round-trip
    // again, this fails even though the run "succeeds".
    let topo: Value = reqwest::get(format!(
        "{}/api/nets/mekhan-{instance_id}/topology",
        engine_url()
    ))
    .await
    .expect("fetch instance net topology")
    .json()
    .await
    .expect("topology json");
    let place_ids: Vec<String> = topo["topology"]["places"]
        .as_array()
        .expect("topology.places")
        .iter()
        .filter_map(|p| p["id"].as_str().map(str::to_string))
        .collect();
    assert!(
        place_ids.iter().any(|p| p == "p_auto_sched_out"),
        "instance net is missing the Scheduled bridge_out `p_auto_sched_out` — \
         the step lowered Inline (deployment_model lost?). places={place_ids:?}"
    );
    assert!(
        !place_ids.iter().any(|p| p == "auto/submitted" || p == "auto/inbox"),
        "instance net has inline executor-lifecycle places — the Scheduled \
         step collapsed to Inline. places={place_ids:?}"
    );

    // Nomad-side guard: a dispatched `petri-executor-worker` child submitted
    // AFTER this test started must exist and its allocation must have
    // actually processed a job — not just exited 0 after idling out
    // (`completed=0`, which is what happens when executor-net publishes to
    // a different namespace than the Nomad worker listens on, letting the
    // dev daemon shadow the work). The tightest signal is the executor's
    // own stdout containing `handling execution job` — only emitted on
    // genuine job processing.
    let nomad_url = std::env::var("TEST_NOMAD_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:4646".to_string());
    let jobs: Value = reqwest::get(format!("{nomad_url}/v1/jobs?prefix=petri-executor-worker"))
        .await
        .expect("fetch Nomad jobs")
        .json()
        .await
        .expect("nomad jobs json");
    let our_jobs: Vec<&Value> = jobs
        .as_array()
        .expect("jobs array")
        .iter()
        .filter(|j| j["ParentID"].as_str() == Some("petri-executor-worker"))
        .filter(|j| j["SubmitTime"].as_i64().unwrap_or(0) > submit_after_nanos)
        .collect();
    assert!(
        !our_jobs.is_empty(),
        "no petri-executor-worker child dispatched after test start — \
         scheduler_submit did not fire (or Nomad backend not registered)"
    );
    let job_id = our_jobs[0]["ID"].as_str().expect("job id").to_string();
    // The parent instance reaches `completed` the moment the result token
    // relays back from the executor (via NATS); the executor OS process is
    // still draining/exiting at that point, so the Nomad alloc takes a few
    // more seconds to transition to `complete`. Poll up to 30s.
    let alloc_deadline = std::time::Instant::now() + Duration::from_secs(30);
    let alloc: Value = loop {
        let allocs: Value = reqwest::get(format!("{nomad_url}/v1/job/{job_id}/allocations"))
            .await
            .expect("fetch allocs")
            .json()
            .await
            .expect("allocs json");
        let arr = allocs.as_array().expect("allocs array").clone();
        if let Some(a) = arr.into_iter().find(|a| {
            matches!(a["ClientStatus"].as_str(), Some("complete") | Some("failed"))
        }) {
            break a;
        }
        if std::time::Instant::now() > alloc_deadline {
            panic!("Nomad alloc for {job_id} never reached terminal state within 30s: {allocs}");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    assert_eq!(
        alloc["ClientStatus"].as_str(),
        Some("complete"),
        "Nomad alloc terminated non-complete: {alloc}"
    );
    let task_failed = alloc["TaskStates"]["petri-worker"]["Failed"]
        .as_bool()
        .unwrap_or(true);
    assert!(!task_failed, "Nomad task petri-worker reported Failed=true: {alloc}");
    let alloc_id = alloc["ID"].as_str().expect("alloc id");
    let stdout = reqwest::get(format!(
        "{nomad_url}/v1/client/fs/logs/{alloc_id}?task=petri-worker&type=stdout&plain=true"
    ))
    .await
    .expect("fetch alloc stdout")
    .text()
    .await
    .expect("alloc stdout text");
    assert!(
        stdout.contains("handling execution job"),
        "Nomad-dispatched executor never processed a job (idle-out, namespace \
         mismatch?). stdout tail:\n{}",
        stdout.lines().rev().take(20).collect::<Vec<_>>().join("\n")
    );

    // Sanity: the parent run produced engine events (the bridge fired).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/v1/instances/{instance_id}/state"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "get instance state");
    let state = body_json(resp.into_body()).await;
    assert!(
        state["event_count"].as_u64().unwrap_or(0) > 0,
        "scheduled run should have produced engine events"
    );
}
