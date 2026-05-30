//! Multi-cluster live e2e: ONE instance leases TWO datacenters of DIFFERENT
//! KINDS (Slurm + Nomad) simultaneously, each holding its own persistent drain
//! executor, with per-step cluster selection.
//!
//! ```text
//!   Start ─▶ Loop[lease=slurm_dc]{ body_s: Scheduled+runOnLease } ─▶
//!           Loop[lease=nomad_dc]{ body_n: Scheduled+runOnLease } ─▶ End
//! ```
//!
//! The two loops lease DIFFERENT datacenter resources — `slurm_dc` (an SSH
//! Slurm cluster, connection carried as an INLINE ssh_key PEM on the resource)
//! and `nomad_dc` (a Nomad cluster, `nomad_addr` on the resource). The engine's
//! `ClusterRegistry` builds BOTH clusters LAZILY from the per-resource
//! connection (NO engine `SLURM_SSH_*` / `NOMAD_ADDR` boot env is load-bearing
//! for the lease path) and runs a per-cluster watcher for each. This is the
//! headline multi-cluster proof: N clusters of any kind, selected per leased
//! loop, observable in `GET /api/clusters`.
//!
//! ── Prerequisites ──────────────────────────────────────────────────────────
//!
//!   just dev slurm-up        # Slurm docker cluster + drain template
//!   (a Nomad agent on :4646 with the `petri-lease-executor` parameterized job)
//!
//! The engine must run with BOTH the `slurm` and `nomad` features (the
//! core-engine defaults), `TMPDIR=/tmp` + `SSH_AUTH_SOCK` unset (so the Slurm
//! SSH ControlMaster socket fits the macOS path limit) and `VAULT_ADDR` /
//! `VAULT_TOKEN` set (so firing-time `resolve_secrets` substitutes the inline
//! ssh_key PEM the Slurm cluster connection rides). The drain executors pull
//! staged `main.py` from the dev rustfs bucket `mekhan-artifacts`, so the same
//! S3 overrides as the other executor-backed e2e apply:
//!
//!   TEST_S3_BUCKET=mekhan-artifacts \
//!   TEST_S3_ENDPOINT=http://localhost:20114 \
//!   TEST_S3_ACCESS_KEY=rustfsadmin TEST_S3_SECRET_KEY=rustfsadmin \
//!   TEST_POSTGRES_URL=postgres://mekhan:mekhan@localhost:20110/mekhan \
//!   TEST_NATS_URL=nats://localhost:20111 \
//!   TEST_PETRI_URL=http://localhost:20101 TEST_ENGINE_URL=http://localhost:20101 \
//!   VAULT_ADDR=http://localhost:20113 VAULT_TOKEN=root \
//!   TEST_SLURM_SSH_KEY=<abs path to engine/infra/slurm/ssh/slurm_test> \
//!   cargo test -p mekhan-service --test scheduled_lease_two_cluster_e2e \
//!       -- --ignored --test-threads=1 --nocapture
//!
//! Ports above are slot-1 (`.dev/slot` = 1); use the slot-0 defaults on the
//! main checkout. Run serially (`--test-threads=1`).

mod common;

use std::collections::BTreeSet;
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

/// Iterations per leased loop. Small — the point is two clusters in flight, not
/// drain throughput (that's `scheduled_lease_{slurm,nomad}_e2e`).
const MAX_ITERATIONS: i32 = 2;

const DC_SLURM_ALIAS: &str = "slurm_dc";
const DC_NOMAD_ALIAS: &str = "nomad_dc";

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

/// A leased loop with a `Scheduled{runOnLease}` Python body, leasing `dc_alias`.
fn leased_loop(loop_id: &str, body_id: &str, dc_alias: &str) -> Vec<WorkflowNode> {
    vec![
        WorkflowNode {
            id: loop_id.to_string(),
            node_type: "loop".to_string(),
            slug: None,
            position: pos(),
            data: WorkflowNodeData::Loop {
                label: format!("Leased Loop ({dc_alias})"),
                description: None,
                max_iterations: MAX_ITERATIONS,
                loop_condition: "true".to_string(),
                accumulators: Vec::<LoopAccumulator>::new(),
                lease: Some(LeaseBinding {
                    scheduler: dc_alias.to_string(),
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
                label: format!("Run Python on lease ({dc_alias})"),
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
                deployment_model: DeploymentModel::Scheduled {
                    scheduler: None,
                    job_template: "mekhan-executor-worker".to_string(),
                    resources: None,
                    operation: ScheduledOperation::Submit,
                    request: None,
                    run_on_lease: true,
                },
            },
            parent_id: Some(loop_id.to_string()),
            width: None,
            height: None,
        },
    ]
}

fn seq(id: &str, source: &str, target: &str, sh: Option<&str>, th: Option<&str>) -> WorkflowEdge {
    WorkflowEdge {
        id: id.to_string(),
        source: source.to_string(),
        target: target.to_string(),
        source_handle: sh.map(str::to_string),
        target_handle: th.map(str::to_string),
        label: None,
        edge_type: "sequence".to_string(),
    }
}

/// `Start → Loop_s{body_s} → Loop_n{body_n} → End`. The two loops lease
/// DIFFERENT datacenters (`slurm_dc`, `nomad_dc`).
fn two_cluster_graph() -> WorkflowGraph {
    let mut nodes = vec![start("s")];
    nodes.extend(leased_loop("lp_s", "body_s", DC_SLURM_ALIAS));
    nodes.extend(leased_loop("lp_n", "body_n", DC_NOMAD_ALIAS));
    nodes.push(end("e"));

    WorkflowGraph {
        nodes,
        edges: vec![
            // Start → Loop_s
            seq("e_in", "s", "lp_s", None, Some("in")),
            // Loop_s body
            seq("e_s_bin", "lp_s", "body_s", Some("body_in"), Some("in")),
            seq("e_s_bout", "body_s", "lp_s", None, Some("body_out")),
            // Loop_s exit → Loop_n entry
            seq("e_mid", "lp_s", "lp_n", None, Some("in")),
            // Loop_n body
            seq("e_n_bin", "lp_n", "body_n", Some("body_in"), Some("in")),
            seq("e_n_bout", "body_n", "lp_n", None, Some("body_out")),
            // Loop_n exit → End
            seq("e_out", "lp_n", "e", None, Some("in")),
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    }
}

const MAIN_PY: &str = r#"log_info("two-cluster leased body ran", task_id=token.get("task_id"))
set_output("ran", True)
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

/// `GET /api/clusters` on the engine → the live `ClusterRegistry` snapshot.
/// Returns `(flavor, active_lease_count, watcher_state)` per cluster.
async fn clusters_snapshot() -> Vec<(String, i64, String)> {
    let Ok(resp) = reqwest::get(format!("{}/api/clusters", engine_url())).await else {
        return vec![];
    };
    let Ok(body) = resp.json::<Value>().await else {
        return vec![];
    };
    body.get("clusters")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    (
                        c.get("flavor").and_then(Value::as_str).unwrap_or("").to_string(),
                        c.get("active_lease_count").and_then(Value::as_i64).unwrap_or(0),
                        c.get("watcher_state")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// Create a datacenter resource via the API and return its `resource_id`.
async fn create_datacenter(
    app: &axum::Router,
    alias: &str,
    display_name: &str,
    config: Value,
) -> Uuid {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/resources")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "path": alias,
                        "resource_type": "datacenter",
                        "display_name": display_name,
                        "config": config,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::CREATED, "create datacenter {alias}: {body}");
    body["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
#[ignore = "live multi-cluster e2e: needs `just dev slurm-up` + a Nomad agent + VAULT_* + TEST_S3_*"]
async fn one_instance_leases_two_clusters_of_different_kinds() {
    if !engine_available().await {
        panic!("engine not available at {} — start the stack", engine_url());
    }

    let engine_nats_url = std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) = common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

    let listener_nats = MekhanNats::connect(&engine_nats_url, None).await.expect("nats");
    let kv = listener_nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("kv");
    let sub_mgr =
        std::sync::Arc::new(SubscriptionManager::new(kv, listener_nats.jetstream().clone()));
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

    // ── (1) Two datacenter resources of different kinds. Both auto-deploy their
    //    `pool-<resource_id>` lease-adapter nets; the engine builds the clusters
    //    lazily from these per-resource connections on first lease.
    let ssh_key_pem = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../engine/infra/slurm/ssh/slurm_test"
    ))
    .expect("read engine/infra/slurm/ssh/slurm_test private key");

    let slurm_id = create_datacenter(
        &app,
        DC_SLURM_ALIAS,
        "Slurm Datacenter (two-cluster e2e)",
        json!({
            "scheduler_flavor": "slurm",
            "ssh_host": std::env::var("TEST_SLURM_SSH_HOST").unwrap_or_else(|_| "localhost".to_string()),
            "ssh_port": std::env::var("TEST_SLURM_SSH_PORT").ok().and_then(|s| s.parse::<u16>().ok()).unwrap_or(2222),
            "ssh_user": std::env::var("TEST_SLURM_SSH_USER").unwrap_or_else(|_| "testuser".to_string()),
            "ssh_known_hosts": "accept",
            "template_dir": std::env::var("TEST_SLURM_TEMPLATE_DIR").unwrap_or_else(|_| "/opt/petri/templates".to_string()),
            "ssh_key": ssh_key_pem
        }),
    )
    .await;

    let nomad_id = create_datacenter(
        &app,
        DC_NOMAD_ALIAS,
        "Nomad Datacenter (two-cluster e2e)",
        json!({
            "scheduler_flavor": "nomad",
            "nomad_addr": std::env::var("TEST_NOMAD_ADDR").unwrap_or_else(|_| "http://localhost:4646".to_string())
        }),
    )
    .await;

    for net_id in [format!("pool-{slurm_id}"), format!("pool-{nomad_id}")] {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !net_running(&net_id).await {
            if Instant::now() > deadline {
                panic!("lease-adapter net `{net_id}` did not reach running within 60s");
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    // ── (2) Publish the two-cluster template.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Two-Cluster Lease E2E",
                        "graph": two_cluster_graph(),
                        "files": {
                            "body_s": { "main.py": MAIN_PY },
                            "body_n": { "main.py": MAIN_PY }
                        },
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

    // ── (3) Launch.
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
                        "metadata": { "e2e": "two_cluster_lease" }
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

    // ── (4) Poll to terminal, sampling `/api/clusters` each tick. The headline
    //    assertion: BOTH a slurm AND a nomad cluster appear in the live
    //    registry, each with an active lease while ITS loop runs. The loops are
    //    sequential, so we accumulate flavors-seen-with-an-active-lease across
    //    the whole run rather than requiring both held at the same instant.
    let mut flavors_with_active_lease: BTreeSet<String> = Default::default();
    let mut flavors_ever_built: BTreeSet<String> = Default::default();

    let deadline = Duration::from_secs(420);
    let started = Instant::now();
    loop {
        let st: String = sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&db)
            .await
            .unwrap();

        for (flavor, active, watcher) in clusters_snapshot().await {
            if flavor == "slurm" || flavor == "nomad" {
                flavors_ever_built.insert(flavor.clone());
                if active > 0 && watcher == "streaming" {
                    flavors_with_active_lease.insert(flavor);
                }
            }
        }

        if st == "completed" {
            break;
        }
        assert_ne!(
            st, "failed",
            "instance failed — a lease/cluster/drain path did not succeed \
             (flavors built so far: {flavors_ever_built:?})"
        );
        if started.elapsed() > deadline {
            panic!(
                "instance did not complete within {deadline:?} (status: {st}; \
                 flavors built: {flavors_ever_built:?}, with-active-lease: \
                 {flavors_with_active_lease:?})"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // ── (5) Assert the multi-cluster invariants.
    assert!(
        flavors_ever_built.contains("slurm") && flavors_ever_built.contains("nomad"),
        "expected BOTH a slurm and a nomad cluster in /api/clusters during the \
         run; saw {flavors_ever_built:?}"
    );
    assert!(
        flavors_with_active_lease.contains("slurm")
            && flavors_with_active_lease.contains("nomad"),
        "expected BOTH clusters to hold an active lease (streaming watcher) while \
         their loop ran; saw {flavors_with_active_lease:?}"
    );

    // Final terminal status sanity.
    let final_status: String =
        sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(final_status, "completed", "instance terminal status");
}
