//! End-to-end coverage for a **loop-scoped Nomad lease** with a PERSISTENT
//! DRAIN EXECUTOR — the Nomad analogue of `scheduled_lease_slurm_e2e.rs`. A
//! leased `Loop` holds ONE Nomad allocation across all its iterations by
//! dispatching ONE long-lived `petri-lease-executor` parameterized job, and
//! every iteration's `runOnLease` body ENQUEUES its job to the lease-scoped
//! NATS namespace that the dispatched drain executor pulls. The executor
//! process is REUSED across all iterations (warm venv/state), not restarted.
//!
//!   parent-net ─▶ datacenter pool-net (lease adapter, scheduler_flavor=nomad)
//!                   ▲ acquire (nomad job dispatch)   │ release (nomad job stop)
//!   Loop(lease) ────┘  └─ ONE drain executor alloc   └──── at terminal exit
//!     │                    (Pool mode, ns=lease-<grant>)
//!     ▼  parked lease at p_lp_data.lease.executor_namespace
//!   body AutomatedStep(runOnLease) ─▶ executor lifecycle enqueue
//!     └─(publish to lease-<grant>.<prio>.<id> on NATS)─▶
//!        the dispatched drain executor pulls + runs it (WARM) ─▶ parent
//!
//! What it proves over the single-Submit Nomad path:
//!   1. The loop acquires EXACTLY ONE Nomad allocation — witnessed by exactly
//!      ONE NEW dispatched child of `petri-lease-executor` for this run.
//!   2. ONE persistent executor drains ALL N jobs — witnessed by that alloc's
//!      `nomad alloc logs` carrying `configuration loaded ... namespace=
//!      lease-<instance>-lp consumer_mode="Pool"` plus >= MAX_ITERATIONS
//!      `handling execution job` lines (the SAME process pulled+ran every
//!      iteration — warm reuse, the payoff over per-job dispatch).
//!   3. The allocation is released on the loop's terminal exit — witnessed by
//!      the dispatched child job going dead/stopped after completion.
//!   4. Topology guard: the loop kept its lease bridges AND the body retargeted
//!      to the executor lifecycle (`body/inbox`, NOT a scheduler `p_body_sched_out`).
//!
//! ── Prerequisites ──
//!
//!   just dev scheduler-up         # Nomad dev agent + engine SCHEDULER_BACKEND=nomad
//!   # register the lease parameterized job (engine/infra/nomad/lease-executor-job-template.json)
//!   # with the native executor binary + slot S3/NATS/SDK env (scheduler-up does
//!   # not yet auto-register the lease job — see the manual bring-up notes).
//!
//! The engine needs `NOMAD_ADDR` so `NomadAllocatorClient::from_env()` is wired,
//! and the datacenter resource is created with `scheduler_flavor = "nomad"`. The
//! dispatched drain executor runs natively (raw_exec) and pulls staged files
//! from the dev rustfs bucket, so the same S3 overrides as the other
//! executor-backed e2e apply:
//!
//!   TEST_S3_BUCKET=mekhan-artifacts TEST_S3_ENDPOINT=http://localhost:20114 \
//!   TEST_S3_ACCESS_KEY=rustfsadmin TEST_S3_SECRET_KEY=rustfsadmin \
//!   TEST_PETRI_URL=http://localhost:20101 TEST_ENGINE_URL=http://localhost:20101 \
//!   TEST_NATS_URL=nats://localhost:20111 ENGINE_NATS_URL=nats://localhost:20111 \
//!   TEST_POSTGRES_URL=postgres://mekhan:mekhan@localhost:20110/mekhan \
//!   cargo test -p mekhan-service --test scheduled_lease_nomad_e2e \
//!       -- --ignored --test-threads=1 --nocapture

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::models::template::{
    default_output_port, DeploymentModel, ExecutionBackendType, ExecutionSpecConfig, LeaseBinding,
    LoopAccumulator, Port, Position, WorkflowEdge, WorkflowGraph, WorkflowNode, WorkflowNodeData,
};
use mekhan_service::nats::MekhanNats;

/// How many loop iterations the leased body runs. The load-bearing assertion is
/// that ALL of them are drained by the same single dispatched executor.
const MAX_ITERATIONS: i32 = 3;

/// Workspace alias of the datacenter resource the loop leases against. Its
/// `scheduler_flavor = "nomad"` routes the engine's `FlavorDispatchAllocatorClient`
/// to the `NomadAllocatorClient` leg.
const DC_ALIAS: &str = "nomad_dc";

/// The parameterized Nomad job the lease dispatches (see `NomadAllocatorClient`).
const LEASE_JOB: &str = "petri-lease-executor";

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

/// `Start → Loop{lease, max_iterations} → End`, with a `Scheduled{runOnLease}`
/// AutomatedStep body parented under the loop. Identical to the Slurm test's
/// graph — the lease binding is backend-agnostic; only the datacenter resource's
/// `scheduler_flavor` decides Slurm vs Nomad.
fn leased_loop_graph(loop_id: &str, body_id: &str) -> WorkflowGraph {
    let scope_id = format!("{loop_id}_scope");
    WorkflowGraph {
        nodes: vec![
            start("s"),
            WorkflowNode {
                id: scope_id.clone(),
                node_type: "lease_scope".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::LeaseScope {
                    label: "Lease Scope".to_string(),
                    description: None,
                    lease: LeaseBinding {
                        scheduler: DC_ALIAS.to_string(),
                        request: None,
                    },
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: loop_id.to_string(),
                node_type: "loop".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::Loop {
                    label: "Leased Loop".to_string(),
                    description: None,
                    max_iterations: MAX_ITERATIONS,
                    loop_condition: "true".to_string(),
                    accumulators: Vec::<LoopAccumulator>::new(),
                },
                parent_id: Some(scope_id.clone()),
                width: None,
                height: None,
            },
            WorkflowNode {
                id: body_id.to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::AutomatedStep {
                    label: "Run Python (Scheduled Nomad, on lease)".to_string(),
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
                    stream_output: false,
                    stream_input: false,
                    deployment_model: DeploymentModel::Scheduled {
                        scheduler: None,
                        job_template: "petri-executor-worker".to_string(),
                        resources: None,
                    },
                },
                parent_id: Some(loop_id.to_string()),
                width: None,
                height: None,
            },
            end("e"),
        ],
        edges: vec![
            WorkflowEdge {
                id: "e_in".to_string(),
                source: "s".to_string(),
                target: scope_id.clone(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
            WorkflowEdge {
                id: "e_scope_body_in".to_string(),
                source: scope_id.clone(),
                target: loop_id.to_string(),
                source_handle: Some("body_in".to_string()),
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
            WorkflowEdge {
                id: "e_body_in".to_string(),
                source: loop_id.to_string(),
                target: body_id.to_string(),
                source_handle: Some("body_in".to_string()),
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
            WorkflowEdge {
                id: "e_body_out".to_string(),
                source: body_id.to_string(),
                target: loop_id.to_string(),
                source_handle: None,
                target_handle: Some("body_out".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
            WorkflowEdge {
                id: "e_loop_body_out".to_string(),
                source: loop_id.to_string(),
                target: scope_id.clone(),
                source_handle: None,
                target_handle: Some("body_out".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
            WorkflowEdge {
                id: "e_out".to_string(),
                source: scope_id.clone(),
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

const MAIN_PY: &str = r#"log_info("leased-loop nomad body iteration ran", task_id=token.get("task_id"))
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

/// Run the `nomad` CLI (against `NOMAD_ADDR`, default `http://localhost:4646`).
/// Returns stdout; panics on non-zero exit so a missing prerequisite is loud.
fn nomad_cli(args: &[&str]) -> String {
    let out = Command::new("nomad")
        .args(args)
        .output()
        .expect("spawn nomad CLI (is `nomad` on PATH + the dev agent up?)");
    if !out.status.success() {
        panic!(
            "nomad {args:?} failed (status={}):\nstdout: {}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

/// The set of dispatched child job IDs of `petri-lease-executor` (every
/// `…/dispatch-<ts>-<hash>` token in `nomad job status`).
fn dispatched_children() -> Vec<String> {
    let out = nomad_cli(&["job", "status", LEASE_JOB]);
    let needle = format!("{LEASE_JOB}/dispatch-");
    let mut ids: Vec<String> = out
        .split_whitespace()
        .filter(|t| t.starts_with(&needle))
        .map(str::to_string)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// The first alloc ID of a dispatched child job, if any.
fn child_alloc_id(child_job: &str) -> Option<String> {
    let out = nomad_cli(&["job", "allocs", "-json", child_job]);
    serde_json::from_str::<Value>(&out)
        .ok()?
        .as_array()?
        .first()?
        .get("ID")?
        .as_str()
        .map(str::to_string)
}

/// `nomad alloc logs <alloc>` stdout (best-effort; empty on transient error).
fn alloc_logs(alloc: &str) -> String {
    Command::new("nomad")
        .args(["alloc", "logs", alloc])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Strip ANSI escapes (the executor logs are colorized).
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for n in chars.by_ref() {
                if n == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[tokio::test]
#[ignore = "live Nomad-lease e2e: needs `just dev scheduler-up` + registered petri-lease-executor + TEST_S3_BUCKET"]
async fn leased_loop_drains_on_one_nomad_alloc() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev scheduler-up`",
            engine_url()
        );
    }
    // The dispatched drain executor uses the executor lifecycle (it enqueues to
    // the lease namespace), so this test does not require pre-deployed infra
    // nets — only the engine's NOMAD_ADDR allocator env and the registered
    // `petri-lease-executor` parameterized job.
    nomad_cli(&["job", "status", LEASE_JOB]); // loud if the lease job is not registered

    let engine_nats_url = std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) = common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

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

    // ── (1) Datacenter resource (scheduler_flavor=nomad). Auto-deploys the
    //    `pool-<resource_id>` lease-adapter net; the actual allocation is a
    //    `nomad job dispatch` from ENGINE env (NOMAD_ADDR) via NomadAllocatorClient.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/resources")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "path": DC_ALIAS,
                        "resource_type": "datacenter",
                        "display_name": "Nomad Datacenter (e2e)",
                        // Multi-cluster: the cluster CONNECTION lives on the
                        // resource (not engine env). The engine's ClusterRegistry
                        // builds a NomadAllocatorClient::from_connection(nomad_addr)
                        // from the effect_config this resource threads.
                        "config": {
                            "scheduler_flavor": "nomad",
                            "nomad_addr": std::env::var("TEST_NOMAD_ADDR")
                                .unwrap_or_else(|_| "http://localhost:4646".to_string())
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let dc_status = resp.status();
    let dc_body = body_json(resp.into_body()).await;
    assert_eq!(
        dc_status,
        StatusCode::CREATED,
        "create datacenter: {dc_body}"
    );
    let resource_id: Uuid = dc_body["id"].as_str().unwrap().parse().unwrap();

    let pool_net_id = format!("pool-{resource_id}");
    let pool_deadline = Instant::now() + Duration::from_secs(60);
    while !net_running(&pool_net_id).await {
        if Instant::now() > pool_deadline {
            panic!("datacenter lease-adapter net `{pool_net_id}` did not reach running within 60s");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // ── (2) Build + publish the leased-loop template.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Leased-Loop Nomad E2E",
                        "graph": leased_loop_graph("lp", "body"),
                        "files": { "body": { "main.py": MAIN_PY } },
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

    // Snapshot the dispatched children BEFORE launch so we can identify THIS
    // run's drain executor as the new one.
    let baseline_children = dispatched_children();

    // ── (3) Launch an instance.
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
                        "metadata": { "e2e": "scheduled_lease_nomad" }
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

    // ── (4) Poll to terminal.
    let deadline = Duration::from_secs(360);
    let started = Instant::now();
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
            "instance failed — Nomad lease/dispatch/executor path did not succeed"
        );
        if started.elapsed() > deadline {
            panic!("instance did not complete within {deadline:?} (status: {st})");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // ── (5) ONE new dispatched drain executor for this run.
    let new_children: Vec<String> = dispatched_children()
        .into_iter()
        .filter(|c| !baseline_children.contains(c))
        .collect();
    assert_eq!(
        new_children.len(),
        1,
        "expected EXACTLY ONE new dispatched `{LEASE_JOB}` child (one lease → one drain \
         executor), saw {}: {new_children:?}",
        new_children.len()
    );
    let drain_child = new_children[0].clone();

    // ── (6) That alloc drained all N iterations, warm — its logs carry the
    //    Pool/drain config for THIS instance's lease namespace plus
    //    >= MAX_ITERATIONS `handling execution job` lines.
    let expected_ns = format!("lease-{instance_id}-lp_scope");
    let out_deadline = Instant::now() + Duration::from_secs(120);
    let (alloc, logs) = loop {
        if let Some(alloc) = child_alloc_id(&drain_child) {
            let logs = strip_ansi(&alloc_logs(&alloc));
            let handled = logs.matches("handling execution job").count();
            if handled >= MAX_ITERATIONS as usize {
                break (alloc, logs);
            }
        }
        if Instant::now() > out_deadline {
            let alloc = child_alloc_id(&drain_child).unwrap_or_default();
            panic!(
                "the dispatched drain executor (child {drain_child}, alloc {alloc}) did not \
                 drain {MAX_ITERATIONS} jobs within 120s of completion. logs tail:\n{}",
                strip_ansi(&alloc_logs(&alloc))
                    .lines()
                    .rev()
                    .take(25)
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let handled = logs.matches("handling execution job").count();
    assert!(
        logs.contains(&format!("namespace={expected_ns}"))
            && logs.contains("consumer_mode=\"Pool\""),
        "drain executor (alloc {alloc}) is not the Pool consumer for this lease's namespace \
         `{expected_ns}` — logs head:\n{}",
        logs.lines().take(5).collect::<Vec<_>>().join("\n")
    );
    assert!(
        handled >= MAX_ITERATIONS as usize,
        "the drain executor at alloc {alloc} handled {handled} jobs, expected >= {MAX_ITERATIONS} \
         (one persistent executor must drain every iteration). logs tail:\n{}",
        logs.lines().rev().take(30).collect::<Vec<_>>().join("\n")
    );

    // ── (7) Topology guard: lease-scope lease places present AND the body
    //    retargeted to the executor lifecycle (`body/inbox`, NOT `p_body_sched_out`).
    //    The lease now lives on the enclosing `LeaseScope` (`lp_scope`), so the
    //    handshake places are scope-namespaced `p_lp_scope_*`.
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
    for required in [
        "p_lp_scope_claim_out",
        "p_lp_scope_grant_inbox",
        "p_lp_scope_register_out",
        "p_lp_scope_release_out",
        "p_lp_scope_held",
    ] {
        assert!(
            place_ids.iter().any(|p| p == required),
            "instance net is missing the lease-scope place `{required}`. places={place_ids:?}"
        );
    }
    assert!(
        place_ids.iter().any(|p| p == "body/inbox"),
        "instance net is missing the executor-lifecycle inbox `body/inbox` — the runOnLease body \
         did not retarget to the executor enqueue path. places={place_ids:?}"
    );
    assert!(
        !place_ids.iter().any(|p| p == "p_body_sched_out"),
        "instance net still has the scheduler bridge_out `p_body_sched_out`. places={place_ids:?}"
    );

    // ── (8) Release: the loop's terminal exit stops the dispatched child
    //    (`nomad job stop`) — its status goes dead/stopped within a deadline.
    let release_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let status_out = nomad_cli(&["job", "status", &drain_child]);
        let dead = status_out
            .lines()
            .any(|l| l.trim_start().starts_with("Status") && l.contains("dead"));
        if dead {
            break;
        }
        if Instant::now() > release_deadline {
            panic!(
                "dispatched drain executor {drain_child} still alive 60s after instance \
                 completion — the terminal exit did not `nomad job stop` it. status:\n{status_out}"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Sanity: the run produced engine events.
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
        "leased-loop nomad run should have produced engine events"
    );
}
