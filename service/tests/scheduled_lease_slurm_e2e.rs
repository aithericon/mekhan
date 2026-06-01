//! End-to-end coverage for a **loop-scoped Slurm lease** with a PERSISTENT
//! DRAIN EXECUTOR — the seam where a leased `Loop` holds ONE Slurm allocation
//! across all its iterations, launches ONE long-lived executor onto that held
//! allocation at acquire, and every iteration's `runOnLease` body simply
//! ENQUEUES its job to the lease-scoped NATS namespace that the held executor
//! drains. The executor process is REUSED across all iterations (warm venv /
//! model / GPU state), not restarted per-iteration.
//!
//!   parent-net ─▶ datacenter pool-net (lease adapter)
//!                   ▲ claim (salloc, once)        │ release (scancel, once)
//!   Loop(lease) ────┘  └─ srun ONE drain executor └──── at terminal exit
//!     │                    on the held alloc (Pool mode, ns=lease-<grant>)
//!     │  parked lease at p_lp_data.lease.executor_namespace
//!     ▼  (read-arc → d.executor_namespace per iteration)
//!   body AutomatedStep(runOnLease) ─▶ executor lifecycle enqueue
//!     └─(publish to lease-<grant>.<prio>.<id> on NATS)─▶
//!        the held drain executor pulls + runs it (WARM) ─▶ result ─▶ parent
//!
//! This is the drain-model counterpart of `scheduled_slurm_e2e.rs` (single
//! Submit, no loop, no lease). What it additionally proves over that test:
//!   1. The loop acquires EXACTLY ONE allocation (one `p_lp_held` token) and
//!      holds it across all `max_iterations` iterations — witnessed by a STABLE
//!      single `squeue --name='petri-<grant_id>'` job id sampled while running.
//!   2. ONE persistent executor (launched once on acquire) drains ALL N jobs —
//!      witnessed by a SINGLE new `/tmp/petri-lease-exec-*.out` file carrying
//!      `Starting lease drain executor` plus N `handling execution job` lines
//!      (the SAME process pulled+ran every iteration's work — warm reuse, the
//!      actual payoff over the old srun-per-iteration model).
//!   3. The allocation is released EXACTLY ONCE on the loop's terminal exit —
//!      witnessed by the `squeue` name going EMPTY after the instance completes
//!      (scancel → SIGTERM → the drain executor exits).
//!   4. Topology regression guard: the loop kept its lease bridges
//!      (`p_lp_claim_out` / `p_lp_grant_inbox` / `p_lp_register_out` /
//!      `p_lp_release_out` / `p_lp_held`) AND the body retargeted to the
//!      executor lifecycle (`body/inbox`, NOT a scheduler `p_body_sched_out`) —
//!      i.e. the loop-lease hoist held and the body now enqueues.
//!
//! ── Prerequisites (identical to `scheduled_slurm_e2e.rs` PLUS a datacenter) ──
//!
//!   just dev slurm-up
//!
//! (Docker Slurm cluster up, `mekhan-lease-executor.sh` (the drain template) +
//! the aithericon Python SDK installed in the container, engine restarted with
//! the SSH allocator env so the lease adapter can `salloc`/`scancel` AND `srun`
//! the drain executor over SSH: `SLURM_SSH_HOST` + `SLURM_SSH_{PORT,USER,KEY,
//! KNOWN_HOSTS}`.) The drain executor pulls the staged `main.py` from the dev
//! rustfs bucket `mekhan-artifacts` via `host.docker.internal`, so this test
//! needs the same S3 overrides as the other executor-backed e2e:
//!
//!   TEST_S3_BUCKET=mekhan-artifacts \
//!   AWS_ENDPOINT_URL=http://localhost:19005 \
//!   AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin \
//!   cargo test -p mekhan-service --test scheduled_lease_slurm_e2e \
//!       -- --ignored --test-threads=1 --nocapture
//!
//! Run serially (`--test-threads=1`) — it shares the live engine / Slurm
//! cluster and the SSH connection. `#[ignore]` so the default `cargo test`
//! lane (no live stack) skips it.

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

/// How many loop iterations the leased body runs. The load-bearing assertion
/// is that ALL of them reuse the same single allocation (one salloc, N srun).
const MAX_ITERATIONS: i32 = 3;

/// Workspace alias (snake_case `IDENT_REGEX`) of the datacenter resource the
/// loop leases against. The loop's `lease.scheduler` names this; publish
/// resolves it to `pool-<resource_id>` + the `Lease__datacenter` schema.
const DC_ALIAS: &str = "slurm_dc";

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
/// AutomatedStep body parented under the loop. The loop holds a Slurm
/// allocation against the `slurm_dc` datacenter resource for the WHOLE run and
/// launches ONE drain executor onto it at acquire; each iteration's body
/// enqueues to that held executor's lease-scoped namespace.
///
/// Node/edge shape follows `lower_loop`'s handle convention:
///   - `Start -> Loop` on the loop's `in` target handle.
///   - `Loop -> body` on the loop's `body_in` SOURCE handle (the inner handle
///     that feeds body children — `loop_.rs` `output_places`
///     `(Some("body_in"), p_body_in)`).
///   - `body -> Loop` on the loop's `body_out` TARGET handle (`loop_.rs`
///     `input_handles["body_out"] -> p_body_out`).
///   - `Loop -> End` on the loop's default (None) source handle (`p_output`,
///     the post-exit outer `out`).
fn leased_loop_graph(loop_id: &str, body_id: &str) -> WorkflowGraph {
    WorkflowGraph {
        nodes: vec![
            start("s"),
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
                    // L3: hold ONE datacenter lease for the whole loop. `request:
                    // None` ⇒ the allocator's default placement (the single-node
                    // dev Slurm cluster). `scheduler` is the datacenter resource's
                    // workspace alias, resolved at publish to its `pool-<id>` net.
                    lease: Some(LeaseBinding {
                        scheduler: DC_ALIAS.to_string(),
                        request: None,
                    }),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: body_id.to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::AutomatedStep {
                    label: "Run Python (Scheduled Slurm, on lease)".to_string(),
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
                    // Drain seam: a plain Scheduled Submit. Because the body's
                    // parent is the leased Loop (lease enclosure BY CONTAINMENT —
                    // no per-step flag), the compiler RE-ROUTES it off the
                    // lease adapter onto the executor lifecycle and stamps
                    // `d.executor_namespace = lp.lease.executor_namespace` into the
                    // body's `prepare`, with the matching Guard read-arc into the
                    // loop's parked `p_lp_data` envelope — so the iteration enqueues
                    // to the lease-scoped namespace the held drain executor pulls.
                    deployment_model: DeploymentModel::Scheduled {
                        scheduler: None,
                        job_template: "mekhan-executor-worker".to_string(),
                        resources: None,
                    },
                },
                // The body ALWAYS sits inside the leasing loop — this parentage
                // is what `enclosing_leased_scope_slug` walks to find `lp`.
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
                target: loop_id.to_string(),
                source_handle: None,
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
                id: "e_out".to_string(),
                source: loop_id.to_string(),
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

const MAIN_PY: &str = r#"log_info("leased-loop slurm body iteration ran", task_id=token.get("task_id"))
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

/// SSH into the dev Slurm container (`testuser@localhost:2222`, keyfile
/// `engine/infra/slurm/ssh/slurm_test`) and run the given remote command.
/// Returns stdout (panics on non-zero exit so a missing prerequisite is loud).
fn slurm_ssh(remote_cmd: &str) -> String {
    let key = std::env::var("TEST_SLURM_SSH_KEY")
        .unwrap_or_else(|_| "engine/infra/slurm/ssh/slurm_test".to_string());
    let out = Command::new("ssh")
        .args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "ConnectTimeout=5",
            "-i",
            &key,
            "-p",
            "2222",
            "testuser@localhost",
            remote_cmd,
        ])
        .output()
        .expect("spawn ssh");
    if !out.status.success() {
        panic!(
            "ssh to slurm container failed (status={}): cmd={remote_cmd:?}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

/// `squeue` for the loop lease's allocation by its grant-derived job name.
/// The allocator stamps `--job-name='petri-<grant_id>'` (`alloc.rs`), and the
/// loop's grant_id is `<instance_id>:<loop_node_id>` (`loop_.rs` grant_id_expr).
/// Returns the (trimmed, sorted) set of matching job ids — `[]` when released.
fn squeue_lease_ids(instance_id: Uuid, loop_id: &str) -> Vec<String> {
    let grant = format!("{instance_id}:{loop_id}");
    let listing = slurm_ssh(&format!(
        "squeue --name='petri-{grant}' -h -o '%i' 2>/dev/null || true"
    ));
    let mut ids: Vec<String> = listing
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[tokio::test]
#[ignore = "live Slurm-lease e2e: needs `just dev slurm-up` + SLURM_SSH_* engine env + TEST_S3_BUCKET"]
async fn leased_loop_holds_one_slurm_alloc_across_iterations() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }
    // NOTE: the drain-model leased body uses the EXECUTOR LIFECYCLE (it enqueues
    // to the lease-scoped namespace drained by the executor srun'd onto the held
    // alloc), NOT a separate cluster dispatch. So unlike `scheduled_slurm_e2e.rs`
    // this test needs no pre-deployed infra nets — only the engine's SLURM_SSH_*
    // allocator env (so acquire can salloc + srun the drain executor) and the
    // `mekhan-lease-executor.sh` drain template installed in the container, both
    // set up by `just dev slurm-up`.

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

    // ── (1) Create the datacenter resource. `create_resource` auto-deploys its
    //    backing lease-adapter net `pool-<resource_id>` (the loop's
    //    claim/grant/register/release bridges target it). Multi-cluster: the
    //    cluster CONNECTION lives on the RESOURCE (not engine env). The engine's
    //    ClusterRegistry lazily builds a `SlurmAllocatorClient::from_connection`
    //    (ssh_host/port/user/known_hosts/template_dir + the inline `ssh_key` PEM,
    //    written to a 0600 tempfile) from the effect_config this resource threads.
    let ssh_key_pem = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../engine/infra/slurm/ssh/slurm_test"
    ))
    .expect("read engine/infra/slurm/ssh/slurm_test private key");
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
                        "display_name": "Slurm Datacenter (e2e)",
                        "config": {
                            "scheduler_flavor": "slurm",
                            "ssh_host": std::env::var("TEST_SLURM_SSH_HOST")
                                .unwrap_or_else(|_| "localhost".to_string()),
                            "ssh_port": std::env::var("TEST_SLURM_SSH_PORT")
                                .ok()
                                .and_then(|s| s.parse::<u16>().ok())
                                .unwrap_or(2222),
                            "ssh_user": std::env::var("TEST_SLURM_SSH_USER")
                                .unwrap_or_else(|_| "testuser".to_string()),
                            "ssh_known_hosts": "accept",
                            "template_dir": std::env::var("TEST_SLURM_TEMPLATE_DIR")
                                .unwrap_or_else(|_| "/opt/petri/templates".to_string()),
                            "ssh_key": ssh_key_pem
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

    // The auto-deployed pool/adapter net id is `pool-<resource_id>` (the same id
    // `resolve_binding` returns and the loop's lease bridges target). Wait for it
    // to be running before publishing/launching the leased template.
    let pool_net_id = format!("pool-{resource_id}");
    let pool_deadline = Instant::now() + Duration::from_secs(60);
    while !net_running(&pool_net_id).await {
        if Instant::now() > pool_deadline {
            panic!(
                "datacenter lease-adapter net `{pool_net_id}` did not reach running within 60s \
                 — ensure_datacenter_adapter_deployed may have failed (engine reachable?)"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // ── (2) Build + publish the leased-loop template. `files` keys map to the
    //    body node id (`body`), staging its `main.py`.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Leased-Loop Slurm E2E",
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

    // Snapshot the drain-executor out-files BEFORE launch — `sacct` is disabled
    // on the dev image, so the set of NEW `/tmp/petri-lease-exec-*.out` files is
    // how we identify THIS run's drain executor unambiguously. The drain launch
    // (`alloc::detached_launch`) redirects the executor's stdout to
    // `/tmp/petri-lease-exec-<alloc_id>.out`.
    let baseline_outs = slurm_ssh("ls /tmp/petri-lease-exec-*.out 2>/dev/null | sort");

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
                        "metadata": { "e2e": "scheduled_lease_slurm" }
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

    // ── (3a) While the run is in flight, sample the lease allocation: assert it
    //    is EXACTLY ONE alloc and STABLE across samples (one salloc, held —
    //    never a per-iteration sbatch). The single `p_lp_held` token is the
    //    structural guarantee; this squeue probe is the runtime witness. We
    //    collect every distinct id seen while status==running.
    let mut seen_alloc_ids: std::collections::BTreeSet<String> = Default::default();
    let mut concurrent_max = 0usize;

    // ── (4) Poll to terminal. Slurm dispatch is SSH+poll based (slow), and we
    //    run N iterations each over an salloc'd step, so allow a generous
    //    deadline. Sample the lease alloc on every poll tick while running.
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
            "instance failed — lease/scheduler/Slurm/executor path did not succeed"
        );
        if started.elapsed() > deadline {
            panic!("instance did not complete within {deadline:?} (status: {st})");
        }
        // Runtime witness of the held allocation. Tolerate transient SSH hiccups
        // by only recording successful probes.
        let ids = squeue_lease_ids(instance_id, "lp");
        concurrent_max = concurrent_max.max(ids.len());
        for id in ids {
            seen_alloc_ids.insert(id);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Across the whole run, AT MOST ONE lease allocation should ever have been
    // live concurrently, and AT MOST ONE distinct alloc id should have appeared
    // (the held salloc, reused). `0` is tolerated only if the run was so fast we
    // never caught the window — the out-file assertion below still proves srun
    // reuse, and the topology guard proves the lease structure. A value > 1 is a
    // hard failure: the loop minted more than one allocation.
    assert!(
        concurrent_max <= 1,
        "loop held more than one concurrent Slurm allocation ({concurrent_max}) — \
         the lease was not hoisted to loop scope (expected exactly one salloc held \
         across all {MAX_ITERATIONS} iterations). alloc ids seen: {seen_alloc_ids:?}"
    );
    assert!(
        seen_alloc_ids.len() <= 1,
        "loop used more than one distinct Slurm allocation id over the run \
         ({:?}) — iterations did NOT reuse a single held alloc (a fresh salloc \
         per iteration is the bug this test guards)",
        seen_alloc_ids
    );

    // ── (5) Topology regression guard. A leased loop whose lease hoist silently
    //    collapsed still "completes", but the instance net lacks the lease
    //    bridges. Assert the loop's lease places exist AND the body retargeted to
    //    the executor lifecycle (`body/inbox`, NOT a scheduler `p_body_sched_out`).
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
        "p_lp_claim_out",
        "p_lp_grant_inbox",
        "p_lp_register_out",
        "p_lp_release_out",
        "p_lp_held",
    ] {
        assert!(
            place_ids.iter().any(|p| p == required),
            "instance net is missing the loop-lease place `{required}` — the \
             loop-scoped lease was not hoisted (the `lease` binding was dropped?). \
             places={place_ids:?}"
        );
    }
    assert!(
        place_ids.iter().any(|p| p == "body/inbox"),
        "instance net is missing the executor-lifecycle inbox `body/inbox` — the \
         runOnLease body did not retarget to the executor enqueue path. \
         places={place_ids:?}"
    );
    assert!(
        !place_ids.iter().any(|p| p == "p_body_sched_out"),
        "instance net still has the scheduler bridge_out `p_body_sched_out` — the \
         runOnLease body did not move off the scheduler dispatch path. places={place_ids:?}"
    );

    // ── (6) WARM-REUSE witness: EXACTLY ONE new `/tmp/petri-lease-exec-*.out`
    //    file (the single persistent drain executor launched on acquire), and it
    //    drained ALL N iteration jobs — its log carries `Starting lease drain
    //    executor` once plus >= MAX_ITERATIONS `handling execution job` lines.
    //    This is the load-bearing improvement over the old srun-per-iteration
    //    model: ONE process (warm venv/state) handled every iteration, instead of
    //    N fresh executors each paying cold-start.
    let out_deadline = Instant::now() + Duration::from_secs(120);
    let (drain_out, drain_log): (String, String) = loop {
        let listing = slurm_ssh("ls /tmp/petri-lease-exec-*.out 2>/dev/null | sort");
        let new_paths: Vec<String> = listing
            .lines()
            .filter(|p| !baseline_outs.lines().any(|b| b == *p))
            .map(str::to_string)
            .collect();
        // The lease launches exactly one drain executor; more than one means the
        // acquire fired multiple times (lease hoist regression).
        assert!(
            new_paths.len() <= 1,
            "expected EXACTLY ONE drain-executor out-file (one srun on acquire), \
             saw {}: {new_paths:?} — the lease acquire fired more than once \
             (the executor was NOT reused across iterations)",
            new_paths.len()
        );
        if let Some(path) = new_paths.first() {
            let log = slurm_ssh(&format!("cat {path} 2>/dev/null || true"));
            let handled = log.matches("handling execution job").count();
            if handled >= MAX_ITERATIONS as usize {
                break (path.clone(), log);
            }
        }
        if Instant::now() > out_deadline {
            let listing2 = slurm_ssh("ls /tmp/petri-lease-exec-*.out 2>/dev/null | sort");
            panic!(
                "the single drain executor did not drain {MAX_ITERATIONS} jobs within 120s \
                 of completion. new out-files: {new_paths:?}. listing: {listing2:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let handled = drain_log.matches("handling execution job").count();
    assert!(
        drain_log.contains("Starting lease drain executor"),
        "drain-executor log at {drain_out} is missing its startup banner — the \
         mekhan-lease-executor.sh template may not have launched. tail:\n{}",
        drain_log
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(
        handled >= MAX_ITERATIONS as usize,
        "the drain executor at {drain_out} handled {handled} jobs, expected \
         >= {MAX_ITERATIONS} (one persistent executor must drain every iteration \
         warm). tail:\n{}",
        drain_log
            .lines()
            .rev()
            .take(30)
            .collect::<Vec<_>>()
            .join("\n")
    );

    // ── (7) Release witness: after the instance completes, the loop's terminal
    //    exit releases the lease EXACTLY ONCE (release_inbox → adapter scancel
    //    <alloc_id>). Assert the lease allocation is GONE — `squeue` for the
    //    grant name goes empty within a deadline.
    let release_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let ids = squeue_lease_ids(instance_id, "lp");
        if ids.is_empty() {
            break;
        }
        if Instant::now() > release_deadline {
            panic!(
                "loop lease allocation {ids:?} still live 60s after instance completion — \
                 the terminal exit did not release (scancel) the held alloc exactly once"
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
        "leased-loop slurm run should have produced engine events"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Fail-fast: when the held lease allocation DIES mid-flight (e.g. the cluster
// preempts/kills it), the leased loop must FAIL FAST — detect the death via the
// per-cluster watcher (held-alloc-death → `lease_failed` signal → `t_lease_abort`
// throws → NetFailed) instead of hanging forever waiting for a body result from
// a dead drain executor. This exercises the failure-routing stamp the allocator
// writes into salloc `--comment` (so the watcher can map the dead alloc back to
// the loop's `lease_failed` place).
//
// REQUIRES SLURM ACCOUNTING (`sacct`). The watcher distinguishes a KILLED alloc
// (CANCELLED/FAILED → JobStatus::Failed → the `lease_failed` route) from a
// normal release ONLY via `sacct`'s terminal state. Without accounting the
// watcher falls back to squeue-disappearance, which infers `Completed` for
// EVERY vanished job and so cannot tell a kill from a release — the failure
// route never fires. The dev docker image (`engine/infra/slurm`) ships with
// `sacct` DISABLED ("Slurm accounting storage is disabled"), so this test only
// passes against a real Slurm cluster with accounting (`slurmdbd`) enabled. The
// fail-fast WIRING (compiler `t_lease_abort`) + the routing stamp are
// offline-covered; the Nomad watcher can prove death-detection live without an
// accounting dependency (it streams allocation events).
// ─────────────────────────────────────────────────────────────────────────────

const SLEEP_PY: &str = r#"import time
log_info("fail-fast body sleeping to hold the lease")
time.sleep(45)
set_output("ran", True)
"#;

/// Create the Slurm datacenter resource (connection-on-resource, inline PEM).
async fn create_slurm_dc(app: &axum::Router, display: &str) -> Uuid {
    let ssh_key_pem = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../engine/infra/slurm/ssh/slurm_test"
    ))
    .expect("read engine/infra/slurm/ssh/slurm_test private key");
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
                        "display_name": display,
                        "config": {
                            "scheduler_flavor": "slurm",
                            "ssh_host": std::env::var("TEST_SLURM_SSH_HOST").unwrap_or_else(|_| "localhost".to_string()),
                            "ssh_port": std::env::var("TEST_SLURM_SSH_PORT").ok().and_then(|s| s.parse::<u16>().ok()).unwrap_or(2222),
                            "ssh_user": std::env::var("TEST_SLURM_SSH_USER").unwrap_or_else(|_| "testuser".to_string()),
                            "ssh_known_hosts": "accept",
                            "template_dir": std::env::var("TEST_SLURM_TEMPLATE_DIR").unwrap_or_else(|_| "/opt/petri/templates".to_string()),
                            "ssh_key": ssh_key_pem
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::CREATED, "create slurm dc: {body}");
    body["id"].as_str().unwrap().parse().unwrap()
}

async fn instance_status(db: &sqlx::PgPool, instance_id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
        .bind(instance_id)
        .fetch_one(db)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "live Slurm-lease fail-fast e2e: needs `just dev slurm-up` + VAULT_* + TEST_S3_* \
            AND Slurm ACCOUNTING (sacct/slurmdbd) enabled — the dev docker image disables it, \
            so the watcher can't distinguish a killed alloc from a release. See the module note."]
async fn leased_loop_fails_fast_when_held_alloc_dies() {
    if !engine_available().await {
        panic!("engine not available at {} — start the stack", engine_url());
    }

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

    let resource_id = create_slurm_dc(&app, "Slurm DC (fail-fast e2e)").await;
    let pool_net_id = format!("pool-{resource_id}");
    let pool_deadline = Instant::now() + Duration::from_secs(60);
    while !net_running(&pool_net_id).await {
        if Instant::now() > pool_deadline {
            panic!("lease-adapter net `{pool_net_id}` did not reach running within 60s");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Publish a leased loop whose body SLEEPS, so the held alloc stays up long
    // enough for us to kill it mid-flight.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Leased-Loop Fail-Fast E2E",
                        "graph": leased_loop_graph("lp", "body"),
                        "files": { "body": { "main.py": SLEEP_PY } },
                        "author_id": Uuid::new_v4(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create template");
    let template_id: Uuid = body_json(resp.into_body()).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

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

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "template_id": template_id, "created_by": Uuid::new_v4(),
                            "metadata": { "e2e": "lease_fail_fast" } })
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

    // Wait for the held alloc to appear (acquire → salloc).
    let alloc_deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let ids = squeue_lease_ids(instance_id, "lp");
        if !ids.is_empty() {
            break;
        }
        let st = instance_status(&db, instance_id).await;
        assert!(
            st != "failed" && st != "completed",
            "instance reached {st} before the held alloc was ever observed"
        );
        if Instant::now() > alloc_deadline {
            panic!("held Slurm alloc never appeared within 120s — acquire did not salloc");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // KILL the held allocation out from under the loop. `scancel` by the
    // grant-derived job name cancels the salloc (and the srun'd drain executor
    // on it) — simulating a cluster-side preemption / node failure.
    let grant = format!("{instance_id}:lp");
    slurm_ssh(&format!(
        "scancel --name='petri-{grant}' 2>/dev/null || true"
    ));

    // Fail-fast assertion: the instance must reach `failed` — NOT hang, NOT
    // complete — within a bounded window (watcher poll ~5s + signal route +
    // t_lease_abort). A `completed` here means the loop ignored the dead lease;
    // a timeout means it HUNG on a dead executor (the bug fail-fast prevents).
    let fail_deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let st = instance_status(&db, instance_id).await;
        if st == "failed" {
            break;
        }
        assert_ne!(
            st, "completed",
            "instance COMPLETED after its held alloc was killed — fail-fast did \
             not trigger (the loop ignored the dead lease / the failure-routing \
             stamp did not reach the watcher)"
        );
        if Instant::now() > fail_deadline {
            panic!(
                "instance did not fail within 120s of killing the held alloc \
                 (status: {st}) — the leased loop HUNG on a dead lease; the \
                 held-alloc-death → lease_failed → t_lease_abort path did not fire"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // No-orphan: the failed loop must have released/cleaned its alloc — squeue
    // for the grant goes empty within a deadline (scancel already removed it;
    // assert it stays gone, i.e. the abort path didn't re-salloc).
    let drain_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if squeue_lease_ids(instance_id, "lp").is_empty() {
            break;
        }
        if Instant::now() > drain_deadline {
            panic!("a Slurm alloc for the failed loop is still live 60s after fail — orphan");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cancel → no orphan: cancelling an instance that is HOLDING a lease must
// actively release (scancel) the held alloc — `cancel_instance` →
// `petri.terminate_net` → the engine's `release_held_leases_for_instance` scans
// the in-use holds and scancels each before tearing the net down. Unlike
// fail-fast this does NOT depend on the watcher / `sacct`: the engine drives the
// scancel directly, so it is provable on the dev image.
// ─────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "live Slurm-lease cancel-no-orphan e2e: needs `just dev slurm-up` + VAULT_* + TEST_S3_*"]
async fn cancelling_a_leased_instance_releases_the_held_alloc() {
    if !engine_available().await {
        panic!("engine not available at {} — start the stack", engine_url());
    }

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

    let resource_id = create_slurm_dc(&app, "Slurm DC (cancel e2e)").await;
    let pool_net_id = format!("pool-{resource_id}");
    let pool_deadline = Instant::now() + Duration::from_secs(60);
    while !net_running(&pool_net_id).await {
        if Instant::now() > pool_deadline {
            panic!("lease-adapter net `{pool_net_id}` did not reach running within 60s");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Leased-Loop Cancel E2E",
                        "graph": leased_loop_graph("lp", "body"),
                        "files": { "body": { "main.py": SLEEP_PY } },
                        "author_id": Uuid::new_v4(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create template");
    let template_id: Uuid = body_json(resp.into_body()).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

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

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({ "template_id": template_id, "created_by": Uuid::new_v4(),
                            "metadata": { "e2e": "lease_cancel" } })
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

    // Wait for the held alloc to appear.
    let alloc_deadline = Instant::now() + Duration::from_secs(120);
    let held_id = loop {
        let ids = squeue_lease_ids(instance_id, "lp");
        if let Some(first) = ids.first() {
            break first.clone();
        }
        let st = instance_status(&db, instance_id).await;
        assert!(
            st != "failed" && st != "completed",
            "instance reached {st} before the held alloc was ever observed"
        );
        if Instant::now() > alloc_deadline {
            panic!("held Slurm alloc never appeared within 120s");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    // CANCEL the instance while it holds the lease.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/instances/{instance_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "cancel instance");
    let cancelled = body_json(resp.into_body()).await;
    assert_eq!(
        cancelled["status"], "cancelled",
        "instance marked cancelled"
    );

    // No-orphan: the held alloc `held_id` must be scancel'd by the engine's
    // `release_held_leases_for_instance` — squeue for the grant goes empty
    // within a deadline. (The drain executor srun'd onto it dies with the alloc.)
    let release_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let ids = squeue_lease_ids(instance_id, "lp");
        if ids.is_empty() {
            break;
        }
        if Instant::now() > release_deadline {
            panic!(
                "Slurm alloc {ids:?} (held {held_id}) still live 60s after cancel — \
                 release_held_leases_for_instance did NOT scancel the held lease \
                 (ORPHAN allocation leaked)"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
