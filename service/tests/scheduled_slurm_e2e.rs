//! End-to-end coverage for a `Scheduled` AutomatedStep dispatched through
//! the **Slurm** scheduler backend — the sibling of `scheduled_e2e.rs` (which
//! exercises the Nomad backend). The compiled parent net uses the datacenter
//! lease adapter pattern; only what `resource_lease_acquire` dispatches to
//! changes (sbatch over SSH instead of Nomad dispatch):
//!
//!   parent-net ─▶ pool-{resource_id} (lease adapter) ─(resource_lease_acquire)─▶
//!                 mekhan-lease-executor (sbatch) ─▶ executor ─▶ result ─▶ parent
//!
//! Why have both? The Nomad and Slurm `ClusterClient` implementations are
//! independent crates (`engine/core-engine/crates/nomad`,
//! `…/crates/slurm`) with non-overlapping failure modes:
//! event-stream vs. poll, HTTP vs. SSH+CLI, JSON job vs. shell template. A
//! green compiler-side lease test does not imply the Slurm path works
//! end-to-end. The Nomad-side analogue caught the `EXECUTOR_NAMESPACE`-
//! mismatch false-execution bug; this is the matching guard for Slurm.
//!
//! Requires the Slurm scheduler layer on top of `just dev up`:
//!
//!   just dev slurm-up
//!
//! (Docker Slurm cluster running, `mekhan-executor-worker.sh` + the
//! aithericon Python SDK installed in the container, engine restarted
//! with `SCHEDULER_BACKEND=slurm`.) The test itself deploys the lease
//! adapter net for the seeded resource. The Slurm-spawned executor pulls
//! the staged main.py from the dev rustfs bucket `mekhan-artifacts` via
//! `host.docker.internal`, so this test needs the same S3 overrides as
//! the other executor-backed e2e. Run serially (`--test-threads=1`) —
//! it shares the live engine/Slurm cluster.

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
                    label: "Run Python (Scheduled Slurm)".to_string(),
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
                        job_template: "mekhan-executor-worker".to_string(),
                        job_template_ref: None,
                        resources: None,
                    },
                    stream_output: false,
                    stream_input: false,
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

const MAIN_PY: &str = r#"log_info("scheduled slurm automated-step e2e ran", task_id=token.get("task_id"))
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

#[tokio::test]
async fn scheduled_automated_step_runs_through_slurm() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }
    // Unified Scheduled needs the Slurm layer; skip if not available.
    // (We probe 'executor-net' as a proxy for the Slurm worker registration).
    if !net_running("executor-net").await {
        println!("SKIPPING scheduled_automated_step_runs_through_slurm: executor-net not deployed");
        return;
    }

    let engine_nats_url = std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) = common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

    // Seed a Slurm datacenter resource.
    let resource_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO resources (id, workspace_id, path, resource_type, display_name, created_by) \
         VALUES ($1, $2, 'local_slurm', 'datacenter', 'Local Slurm', $3)"
    )
    .bind(resource_id)
    .bind(Uuid::nil())
    .bind(Uuid::nil())
    .execute(&db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO resource_versions (resource_id, version, vault_path, public_config, created_by) \
         VALUES ($1, 1, 'secret/testing/slurm', $2, $3)"
    )
    .bind(resource_id)
    .bind(json!({
        "scheduler_flavor": "slurm",
        "ssh_host": "localhost",
        "ssh_port": 2222,
        "ssh_user": "testuser",
        "template_dir": "/opt/petri/templates",
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

    // Deploy the datacenter lease adapter net for our seeded Slurm resource.
    let conn = mekhan_service::petri::pool_net::DatacenterConnection {
        resource_id,
        resource_version: 1,
        scheduler_flavor: "slurm".to_string(),
        allocator_url: None,
        token_secret_ref: None,
        ssh_host: Some("localhost".to_string()),
        ssh_port: Some(2222),
        ssh_user: Some("testuser".to_string()),
        ssh_known_hosts: Some("accept".to_string()),
        template_dir: Some("/opt/petri/templates".to_string()),
        ssh_key_secret_ref: Some("{{secret:testing/slurm#ssh_key}}".to_string()),
        nomad_addr: None,
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

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Scheduled-Slurm AutomatedStep E2E",
                        "graph": scheduled_graph("auto", "local_slurm"),
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

    // The upstream `nathanhess/slurm:full-root` image disables Slurm
    // accounting (`AccountingStorageType=accounting_storage/none`), so
    // `sacct` cannot see completed jobs — we can't use it to identify
    // "the job dispatched by THIS test". Instead, snapshot the set of
    // `/tmp/petri-executor-*.out` files (set by `#SBATCH --output=` in
    // the template) BEFORE instance creation; whatever new file appears
    // after completion is unambiguously ours. The watcher itself
    // tolerates missing sacct via its tracked-jobs persistence (see
    // engine/core-engine/crates/slurm/src/watcher.rs).
    let baseline_outs = slurm_ssh("ls /tmp/petri-executor-*.out 2>/dev/null | sort");

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
                        "metadata": { "e2e": "scheduled_slurm_automated_step" }
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

    // Slurm dispatch goes via SSH + sbatch + poll-based watcher (2s default),
    // so the round-trip is meaningfully slower than Nomad's event stream.
    let deadline = Duration::from_secs(240);
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
            "instance failed — lease-adapter/Slurm/executor path did not succeed"
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

    // Slurm-side guard: identify the job dispatched by THIS test
    // (whichever `/tmp/petri-executor-*.out` is new vs the pre-test
    // snapshot) and prove its executor genuinely processed work, not
    // just idled out. The strongest signal is the same as the Nomad
    // case: the executor's own stdout containing `handling execution
    // job` — present only on a real job pull. Without this we cannot
    // distinguish "Slurm dispatched + executor exited 0 from idle
    // timeout" (the `EXECUTOR_NAMESPACE=executor_jobs` trap the recipe
    // explicitly fixes) from "Slurm dispatched + executor pulled and
    // ran the job".
    let alloc_deadline = Instant::now() + Duration::from_secs(60);
    let new_out_path = loop {
        let listing = slurm_ssh("ls /tmp/petri-executor-*.out 2>/dev/null | sort");
        let new_paths: Vec<&str> = listing
            .lines()
            .filter(|p| !baseline_outs.lines().any(|b| b == *p))
            .collect();
        if let Some(p) = new_paths.last().copied() {
            // Wait until the file looks done writing — the executor
            // appends `Starting executor` then runs the job; we expect
            // at least the header to be there.
            let size: u64 = slurm_ssh(&format!("stat -c %s {p} 2>/dev/null || echo 0"))
                .trim()
                .parse()
                .unwrap_or(0);
            if size > 0 {
                break p.to_string();
            }
        }
        if Instant::now() > alloc_deadline {
            panic!(
                "no new /tmp/petri-executor-*.out file appeared within 60s \
                 of instance completion — resource_lease_acquire may not have \
                 fired, or the Slurm backend was not registered (engine \
                 env missing?). Last listing: {listing:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };

    let stdout = slurm_ssh(&format!("cat {new_out_path} 2>/dev/null || true"));
    assert!(
        stdout.contains("handling execution job"),
        "Slurm-dispatched executor at {new_out_path} never processed work \
         (idle-out → namespace mismatch?). stdout tail:\n{}",
        stdout.lines().rev().take(20).collect::<Vec<_>>().join("\n")
    );

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
        "scheduled-slurm run should have produced engine events"
    );
}
