//! End-to-end coverage for a `Scheduled` AutomatedStep using the datacenter
//! lease adapter pattern (real Nomad), the one keystone feature with no prior
//! runtime proof.
//!
//! Unlike an Inline step (which the compiler lowers to a direct
//! executor-lifecycle), a Scheduled step compiles to a lease lifecycle —
//! claiming capacity from a per-resource `pool-{resource_id}` adapter net
//! via `resource_lease_acquire`, then enqueuing work to the held allocation's
//! drain executor. The full path exercised here:
//!
//!   parent-net ─▶ pool-{resource_id} (lease adapter) ─(resource_lease_acquire)─▶
//!                 petri-lease-executor (drain) ─▶ executor ─▶ result ─▶ parent
//!
//! This is the runtime counterpart to the static compiler tests proving the
//! lease lifecycle lowering (claim → grant → register → held → release).
//!
//! Requires the Nomad scheduler layer on top of `just dev up`:
//!
//!   just dev scheduler-up
//!
//! (Nomad agent :4646, petri-executor-worker + petri-lease-executor registered,
//! engine restarted with SCHEDULER_BACKEND=nomad.) The test itself deploys the
//! lease adapter net for the seeded resource. The Nomad-spawned executor pulls
//! the staged main.py from the dev rustfs bucket `mekhan-artifacts`, so this
//! test needs the same S3 overrides as the other executor-backed e2e. Run
//! serially (`--test-threads=1`) — it shares the live engine/executor/Nomad.

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

/// `Start → AutomatedStep(python, Scheduled via a datacenter lease) → End`.
fn scheduled_graph(step_id: &str, scheduler: &str) -> WorkflowGraph {
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
                    // Standalone Scheduled step: now performs a single-node lease.
                    deployment_model: DeploymentModel::Scheduled {
                        scheduler: Some(scheduler.to_string()),
                        job_template: "petri-executor-worker".to_string(),
                        resources: None,
                    },
                    stream_output: false,
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
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    }
}

/// Minimal Aithericon-SDK Python step (same contract as the inline e2e): the
/// runner injects `set_output` / `log_info` / `token` as globals.
const MAIN_PY: &str = r#"log_info("scheduled automated-step e2e ran", task_id=token.get("task_id"))
set_output("ran", True)
set_output("answer", 42)
"#;

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:13030".to_string())
}

async fn engine_available() -> bool {
    matches!(
        reqwest::get(format!("{}/api/nets/metadata", engine_url())).await,
        Ok(resp) if resp.status().is_success()
    )
}

async fn net_running(net_id: &str) -> bool {
    match reqwest::get(format!("{}/api/nets/{net_id}/state", engine_url())).await {
        Ok(resp) if resp.status().is_success() => {
            resp.json::<Value>()
                .await
                .ok()
                .and_then(|v| {
                    v.get("run_mode")
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .as_deref()
                == Some("running")
        }
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
    // Unified Scheduled needs the Nomad layer; skip if not available.
    // (We probe 'executor-net' as a proxy for the Nomad worker registration).
    if !net_running("executor-net").await {
        println!("SKIPPING scheduled_automated_step_runs_through_nomad: executor-net not deployed");
        return;
    }

    let engine_nats_url = std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) = common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

    // Seed a Nomad datacenter resource.
    let resource_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO resources (id, workspace_id, path, resource_type, display_name, created_by) \
         VALUES ($1, $2, 'local_nomad', 'datacenter', 'Local Nomad', $3)"
    )
    .bind(resource_id)
    .bind(Uuid::nil())
    .bind(Uuid::nil())
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO resource_versions (resource_id, version, vault_path, public_config, created_by) \
         VALUES ($1, 1, 'secret/testing/nomad', $2, $3)"
    )
    .bind(resource_id)
    .bind(json!({
        "scheduler_flavor": "nomad",
        "nomad_addr": std::env::var("TEST_NOMAD_URL").unwrap_or_else(|_| "http://localhost:4646".to_string()),
    }))
    .bind(Uuid::nil())
    .execute(&db)
    .await
    .unwrap();

    let listener_nats = MekhanNats::connect(&engine_nats_url, None)
        .await
        .expect("nats");
    let kv = listener_nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("kv");
    let sub_mgr = std::sync::Arc::new(SubscriptionManager::new(
        kv,
        listener_nats.jetstream().clone(),
    ));

    // Deploy the datacenter lease adapter net for our seeded resource.
    // In production, mekhan-service deploys this automatically on resource create/publish.
    let nomad_url = std::env::var("TEST_NOMAD_URL").unwrap_or_else(|_| "http://localhost:4646".to_string());
    let conn = mekhan_service::petri::pool_net::DatacenterConnection {
        resource_id,
        resource_version: 1,
        scheduler_flavor: "nomad".to_string(),
        allocator_url: None,
        token_secret_ref: None,
        ssh_host: None,
        ssh_port: None,
        ssh_user: None,
        ssh_known_hosts: None,
        template_dir: None,
        ssh_key_secret_ref: None,
        nomad_addr: Some(nomad_url),
        nomad_region: None,
        nomad_token_secret_ref: None,
    };
    let adapter_air = mekhan_service::petri::pool_net::build_datacenter_lease_adapter_net(&conn);
    let net_id = format!("pool-{resource_id}");
    let deploy_resp = reqwest::Client::new()
        .post(format!("{}/api/nets/{net_id}/scenario", engine_url()))
        .json(&adapter_air)
        .send()
        .await
        .expect("deploy lease adapter");
    assert_eq!(deploy_resp.status(), StatusCode::OK, "deploy lease adapter failed");
    let activate_resp = reqwest::Client::new()
        .put(format!("{}/api/nets/{net_id}/run-mode", engine_url()))
        .json(&serde_json::json!({"mode": "running"}))
        .send()
        .await
        .expect("activate lease adapter");
    assert_eq!(activate_resp.status(), StatusCode::OK, "activate lease adapter failed");
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
                        "graph": scheduled_graph("auto", "local_nomad"),
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
    // target a *deployed* pool-{resource_id}/claim_inbox or this 422s.
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
    assert_eq!(
        inst_status,
        StatusCode::CREATED,
        "create instance: {instance}"
    );
    let instance_id: Uuid = instance["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(instance["status"], "running");

    // The lease adapter acquires a Nomad allocation (petri-lease-executor),
    // which spawns a persistent drain executor; it pulls main.py from S3,
    // runs python3, the result relays back through the lease lifecycle to
    // the parent's reply channel, and the parent net runs to End. Nomad
    // dispatch + cold executor is slower than the inline path, so allow a
    // generous deadline.
    let deadline = Duration::from_secs(180);
    let started = std::time::Instant::now();
    loop {
        let st: String = sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&db)
            .await
            .unwrap();
        if st == "completed" {
            break;
        }
        assert_ne!(
            st, "failed",
            "instance failed — lease-adapter/Nomad/executor path did not succeed"
        );
        if started.elapsed() > deadline {
            panic!("instance did not complete within {deadline:?} (status: {st})");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Regression guard: "instance completes" is *also* true if the Scheduled
    // step silently collapsed to Inline. So assert the deployed instance net
    // actually carries the POOLED lowering — the `p_auto_claim_out` bridge_out
    // to the datacenter adapter net — and NOT the inline executor-lifecycle
    // places (`auto/submitted`, `auto/inbox`).
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
        place_ids.iter().any(|p| p == "p_auto_claim_out"),
        "instance net is missing the pooled bridge_out `p_auto_claim_out` — \
         the step did not use the unified lease path. places={place_ids:?}"
    );

    // Nomad-side guard: a dispatched `petri-executor-worker` child submitted
    // AFTER this test started must exist and its allocation must have
    // actually processed a job — not just exited 0 after idling out
    // (`completed=0`, which is what happens when executor-net publishes to
    // a different namespace than the Nomad worker listens on, letting the
    // dev daemon shadow the work). The tightest signal is the executor's
    // own stdout containing `handling execution job` — only emitted on
    // genuine job processing.
    let nomad_url =
        std::env::var("TEST_NOMAD_URL").unwrap_or_else(|_| "http://127.0.0.1:4646".to_string());
    let jobs: Value = reqwest::get(format!("{nomad_url}/v1/jobs?prefix=petri-lease-executor"))
        .await
        .expect("fetch Nomad jobs")
        .json()
        .await
        .expect("nomad jobs json");
    let our_jobs: Vec<&Value> = jobs
        .as_array()
        .expect("jobs array")
        .iter()
        .filter(|j| j["ParentID"].as_str() == Some("petri-lease-executor"))
        .filter(|j| j["SubmitTime"].as_i64().unwrap_or(0) > submit_after_nanos)
        .collect();
    assert!(
        !our_jobs.is_empty(),
        "no petri-lease-executor child dispatched after test start — \
         lease_acquire did not fire (or Nomad backend not registered)"
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
            matches!(
                a["ClientStatus"].as_str(),
                Some("complete") | Some("failed")
            )
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
    let task_failed = alloc["TaskStates"]["petri-lease-worker"]["Failed"]
        .as_bool()
        .unwrap_or(true);
    assert!(
        !task_failed,
        "Nomad task petri-lease-worker reported Failed=true: {alloc}"
    );
    let alloc_id = alloc["ID"].as_str().expect("alloc id");
    let stdout = reqwest::get(format!(
        "{nomad_url}/v1/client/fs/logs/{alloc_id}?task=petri-lease-worker&type=stdout&plain=true"
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
