//! End-to-end coverage for a **loop-scoped Slurm lease** (L4) — the seam where
//! a leased `Loop` holds ONE Slurm allocation across all its iterations and
//! every iteration's `Scheduled { runOnLease: true }` body `srun`s onto that
//! held allocation instead of `sbatch`-ing a fresh job.
//!
//!   parent-net ─▶ datacenter pool-net (lease adapter)
//!                   ▲ claim (salloc, once)        │ release (scancel, once)
//!   Loop(lease) ────┘                             └──── at terminal exit
//!     │  held alloc_id parked at p_lp_data.lease.alloc_id
//!     ▼  (read-arc → d.spec.alloc_id per iteration)
//!   body AutomatedStep(Scheduled, runOnLease) ─▶ scheduler-net
//!     └─(SlurmClient::submit sees spec.alloc_id ⇒ srun --jobid=<alloc>)─▶
//!        executor (in the SAME held Slurm alloc) ─▶ result ─▶ parent
//!
//! This is the L4 counterpart of `scheduled_slurm_e2e.rs` (single Submit, no
//! loop, no lease). What it additionally proves over that test:
//!   1. The loop acquires EXACTLY ONE allocation (one `p_lp_held` token) and
//!      holds it across all `max_iterations` iterations — witnessed by a STABLE
//!      single `squeue --name='petri-<grant_id>'` job id sampled while running.
//!   2. Each iteration `srun`s into that held alloc (NOT a fresh `sbatch`) —
//!      witnessed by N new `/tmp/petri-srun-*.out` files all carrying
//!      `handling execution job` (the executor really pulled+ran work).
//!   3. The allocation is released EXACTLY ONCE on the loop's terminal exit —
//!      witnessed by the `squeue` name going EMPTY after the instance completes.
//!   4. Topology regression guard: the loop kept its lease bridges
//!      (`p_lp_claim_out` / `p_lp_grant_inbox` / `p_lp_register_out` /
//!      `p_lp_release_out` / `p_lp_held`) AND the body kept its Scheduled
//!      bridge (`p_body_sched_out`, not an inline `body/inbox`) — i.e. neither
//!      the loop-lease hoist nor the Scheduled body collapsed.
//!
//! ── Prerequisites (identical to `scheduled_slurm_e2e.rs` PLUS a datacenter) ──
//!
//!   just dev slurm-up
//!
//! (Docker Slurm cluster up, `mekhan-executor-worker.sh` + the aithericon
//! Python SDK installed in the container, engine restarted with
//! `SCHEDULER_BACKEND=slurm` AND the SSH allocator env so the lease adapter can
//! `salloc`/`scancel` over SSH: `SLURM_SSH_HOST` + `SLURM_SSH_{PORT,USER,KEY,
//! KNOWN_HOSTS}`, scheduler-net + executor-net deployed & running.) The
//! Slurm-spawned executor pulls the staged `main.py` from the dev rustfs bucket
//! `mekhan-artifacts` via `host.docker.internal`, so this test needs the same
//! S3 overrides as the other executor-backed e2e:
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
    LoopAccumulator, Port, Position, ScheduledOperation, WorkflowEdge, WorkflowGraph, WorkflowNode,
    WorkflowNodeData,
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
/// allocation against the `slurm_dc` datacenter resource for the WHOLE run;
/// each iteration's body `srun`s onto that held alloc.
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
                    // L4 seam: Scheduled Submit + `runOnLease`. Because the body's
                    // parent is the leased Loop, the compiler injects
                    // `d.spec.alloc_id = lp.lease.alloc_id` into `t_<body>_prepare`
                    // and registers the matching Guard read-arc into the loop's
                    // parked `p_lp_data` envelope — so the engine srun's onto the
                    // held alloc rather than sbatch-ing a fresh job.
                    deployment_model: DeploymentModel::Scheduled {
                        scheduler: None,
                        job_template: "mekhan-executor-worker".to_string(),
                        resources: None,
                        operation: ScheduledOperation::Submit,
                        request: None,
                        run_on_lease: true,
                    },
                },
                // The body ALWAYS sits inside the leasing loop — this parentage
                // is what `enclosing_leased_loop_slug` walks to find `lp`.
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
    if !net_running("scheduler-net").await || !net_running("executor-net").await {
        panic!("scheduler-net / executor-net not deployed+running — run `just dev slurm-up`");
    }

    let engine_nats_url = std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) = common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

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

    // ── (1) Create the datacenter resource. `create_resource` auto-deploys its
    //    backing lease-adapter net `pool-<resource_id>` (the loop's
    //    claim/grant/register/release bridges target it). For
    //    `scheduler_flavor = "slurm"` the actual allocation goes over SSH from
    //    ENGINE env (`SLURM_SSH_HOST` etc.) via `SlurmAllocatorClient`, so
    //    `allocator_url` / `token` are placeholders on the slurm leg — they are
    //    only load-bearing for the generic `"http"` flavor.
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
                            "allocator_url": "http://unused",
                            "scheduler_flavor": "slurm",
                            "token": "unused"
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
    assert_eq!(dc_status, StatusCode::CREATED, "create datacenter: {dc_body}");
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

    // Snapshot the executor out-files BEFORE launch — see the rationale in
    // `scheduled_slurm_e2e.rs`: `sacct` is disabled on the dev image, so the
    // set of NEW `/tmp/petri-srun-*.out` files is how we identify THIS
    // run's per-iteration srun steps unambiguously.
    let baseline_outs = slurm_ssh("ls /tmp/petri-srun-*.out 2>/dev/null | sort");

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
    //    collapsed (or a Scheduled body that lowered Inline) still "completes",
    //    but the instance net lacks the lease bridges / the Scheduled bridge_out.
    //    Assert the loop's lease places exist AND the body kept its Scheduled
    //    bridge (NOT an inline `body/inbox`).
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
        place_ids.iter().any(|p| p == "p_body_sched_out"),
        "instance net is missing the Scheduled bridge_out `p_body_sched_out` — \
         the body step lowered Inline (deployment_model lost?). places={place_ids:?}"
    );
    assert!(
        !place_ids.iter().any(|p| p == "body/submitted" || p == "body/inbox"),
        "instance net has inline executor-lifecycle places — the Scheduled body \
         collapsed to Inline. places={place_ids:?}"
    );

    // ── (6) srun reuse witness: N new `/tmp/petri-srun-*.out` files (one per
    //    iteration), each carrying `handling execution job` (the executor really
    //    pulled+ran work on the leased nodes — not an idle-out namespace-mismatch
    //    no-op). This is the per-iteration analogue of the single-job assertion
    //    in `scheduled_slurm_e2e.rs`.
    let out_deadline = Instant::now() + Duration::from_secs(90);
    let new_outs: Vec<String> = loop {
        let listing = slurm_ssh("ls /tmp/petri-srun-*.out 2>/dev/null | sort");
        let new_paths: Vec<String> = listing
            .lines()
            .filter(|p| !baseline_outs.lines().any(|b| b == *p))
            .map(str::to_string)
            .collect();
        if new_paths.len() >= MAX_ITERATIONS as usize {
            break new_paths;
        }
        if Instant::now() > out_deadline {
            panic!(
                "expected {MAX_ITERATIONS} new /tmp/petri-srun-*.out files (one srun per \
                 iteration) within 90s of completion, saw {}: {new_paths:?}. \
                 Last listing: {listing:?}",
                new_paths.len()
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    for out_path in &new_outs {
        let stdout = slurm_ssh(&format!("cat {out_path} 2>/dev/null || true"));
        assert!(
            stdout.contains("handling execution job"),
            "Slurm-leased executor at {out_path} never processed work (idle-out → \
             namespace mismatch, or srun-into-alloc did not pull the job). stdout tail:\n{}",
            stdout.lines().rev().take(20).collect::<Vec<_>>().join("\n")
        );
    }

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
