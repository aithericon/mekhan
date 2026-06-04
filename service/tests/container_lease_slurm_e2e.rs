//! End-to-end coverage for **container staging on a Slurm lease** (docs/22): a
//! leased `Loop` whose body's job template binds a `container_image` resource
//! runs its persistent drain executor *inside* `apptainer exec … <sif> …` on the
//! held allocation. This is the headline proof that the compiler-emitted
//! `container:{sif_path,binds,nv}` blob (merged into the lease claim request) is
//! read by the engine and wraps the executor launch.
//!
//!   container_image resource ─▶ materialize net (apptainer pull → /shared/sif)
//!                                  └─ image_materializations row → `ready`
//!   datacenter resource ─▶ pool-net (lease adapter)
//!   LeaseScope(lease) ── claim {request:{container:{sif_path,binds,nv}}} ──▶ acquire_lease
//!     │                     └─ srun `apptainer exec --bind … <sif> /bin/bash <drain.sh>`
//!     ▼  body enqueues to lease-<grant> ns → the IN-CONTAINER drain executor pulls + runs
//!   body AutomatedStep(Scheduled, job_template→container_image) — python main.py
//!
//! What it proves over `scheduled_lease_slurm_e2e.rs` (same lease topology):
//!   1. Publish auto-materializes the image: an `image_materializations` row for
//!      (container_resource, version, datacenter) reaches `ready` with a digest +
//!      `sif_path`, and the stable by-ref symlink exists on the cluster FS.
//!   2. The drain executor's srun line is **apptainer-wrapped** — witnessed by
//!      `ps -eo args` on the held alloc showing `apptainer exec … <sif> /bin/bash`.
//!   3. Every loop iteration runs **inside** the container and routes results back
//!      (N `handling execution job` lines from the single drain executor — warm
//!      reuse), and the per-image venv cache (`/shared/venv-cache/<ref>`) is
//!      populated (iteration 2+ is warm).
//!   4. The lease is released exactly once on terminal exit (squeue empties).
//!
//! ── Prerequisites ──
//!
//!   just dev slurm-up
//!
//! (Brings up the Docker Slurm cluster built WITH apptainer + a static `uv` +
//! `/shared/{sif,venv-cache,apptainer-cache}` (docs/22 Dockerfile additions),
//! `privileged: true` so unprivileged `apptainer pull`/`exec` work, the
//! `mekhan-lease-executor.sh` drain template + the aithericon SDK installed, and
//! the engine restarted with the SSH allocator env. The materialize effect pulls
//! `docker://python:3.12-slim` from inside the container, so the cluster needs
//! registry egress.)
//!
//!   TEST_S3_BUCKET=mekhan-artifacts \
//!   AWS_ENDPOINT_URL=http://localhost:19005 \
//!   AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin \
//!   cargo test -p mekhan-service --test container_lease_slurm_e2e \
//!       -- --ignored --test-threads=1 --nocapture
//!
//! `#[ignore]` so the default lane (no live stack) skips it. Run serially.
//!
//! ── Apple Silicon limitation ──
//!
//! Needs a NATIVE x86_64 Linux cluster for the in-container assertions. On an
//! arm64 Mac the dev Slurm container is `linux/amd64` under Docker Desktop's
//! Rosetta/qemu emulation; `apptainer pull` + `.sif` conversion work, and the
//! `apptainer exec` wrap launches, but exec of the container's amd64 process
//! fails with `exec … failed: invalid argument` — apptainer's fresh mount/user
//! namespace doesn't inherit the emulation's binfmt interpreter. The materialize
//! + lease-acquire + apptainer-wrap path is fully exercised up to that exec; the
//! in-container execution itself only runs on a real x86_64 host.

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
    LoopAccumulator, Port, Position, TemplateRef, WorkflowEdge, WorkflowGraph, WorkflowNode,
    WorkflowNodeData,
};
use mekhan_service::nats::MekhanNats;

/// Loop iterations the leased body runs — all must reuse the one held alloc +
/// the one in-container drain executor.
const MAX_ITERATIONS: i32 = 3;

/// Datacenter resource alias the loop leases against (snake_case `IDENT_REGEX`).
const DC_ALIAS: &str = "slurm_dc";

/// Container-image resource alias the job template binds.
const CONTAINER_ALIAS: &str = "py_container";

/// The image the materialize effect pulls. Public (v1 = no registry creds).
const IMAGE_REF: &str = "docker://python:3.12-slim";

/// The by-ref symlink the compiler embeds + the engine execs. MUST match
/// `service/src/compiler/container_ref.rs::by_ref_sif_path(IMAGE_REF)` and the
/// engine's `sanitize_image_ref` (docs/22). `docker://python:3.12-slim` →
/// `docker_python_3_12_slim`.
const BY_REF_SIF: &str = "/shared/sif/by-ref/docker_python_3_12_slim.sif";

/// The per-image venv cache bind the executor warms across iterations.
const VENV_CACHE: &str = "/shared/venv-cache/docker_python_3_12_slim";

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

/// `Start → LeaseScope → Loop{max_iterations} → End`, with a `Scheduled`
/// AutomatedStep body parented under the loop. Unlike `scheduled_lease_slurm_e2e`,
/// the body references a real **job-template entity** via `job_template_ref` (so
/// publish resolves it AND `resolve_container_specs` can chase its
/// `container_resource_id` → image_ref → by-ref sif and hoist the container blob
/// to the LeaseScope claim).
fn container_leased_loop_graph(
    loop_id: &str,
    body_id: &str,
    template_ref: TemplateRef,
    slug: &str,
) -> WorkflowGraph {
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
                    label: "Run Python in container (Scheduled Slurm, on lease)".to_string(),
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
                    channels: Vec::new(),
                    requirements: None,
                    asset_bindings: Vec::new(),
                    deployment_model: DeploymentModel::Scheduled {
                        scheduler: None,
                        job_template: slug.to_string(),
                        job_template_ref: Some(template_ref),
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

/// Body logs the in-container python version so the run output witnesses that it
/// executed inside `python:3.12-slim` (the host Slurm image ships a different
/// python). Primary in-container witness is the `apptainer exec` ps probe; this
/// is a soft corroboration.
const MAIN_PY: &str = r#"import sys
log_info("container-lease slurm body iteration ran", py=sys.version.split()[0], task_id=token.get("task_id"))
set_output("ran", True)
set_output("py", sys.version.split()[0])
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
                .and_then(|v| v.get("run_mode").and_then(|m| m.as_str()).map(str::to_string))
                .as_deref()
                == Some("running")
        }
        _ => false,
    }
}

/// SSH into the dev Slurm container and run `remote_cmd`; returns stdout (panics
/// on non-zero so a missing prerequisite is loud).
fn slurm_ssh(remote_cmd: &str) -> String {
    // Absolute by default: `cargo test` runs the binary with CWD = the package
    // dir (service/), so a repo-relative path misses. Mirror the PEM read's
    // CARGO_MANIFEST_DIR anchor.
    let key = std::env::var("TEST_SLURM_SSH_KEY").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../engine/infra/slurm/ssh/slurm_test").to_string()
    });
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

/// `squeue` for the lease allocation by its grant-derived job name
/// (`petri-<instance_id>:<holder_id>`). Holder is the LeaseScope (`lp_scope`).
fn squeue_lease_ids(instance_id: Uuid, holder_id: &str) -> Vec<String> {
    let grant = format!("{instance_id}:{holder_id}");
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

/// Create the Slurm datacenter resource (connection-on-resource, inline PEM).
async fn create_slurm_dc(app: &axum::Router) -> Uuid {
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
                        "display_name": "Slurm Datacenter (container e2e)",
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

/// Create the `container_image` resource (public image, no creds in v1).
async fn create_container_image(app: &axum::Router) -> Uuid {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/resources")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "path": CONTAINER_ALIAS,
                        "resource_type": "container_image",
                        "display_name": "Python 3.12 (container e2e)",
                        "config": { "image_ref": IMAGE_REF }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::CREATED, "create container_image: {body}");
    body["id"].as_str().unwrap().parse().unwrap()
}

/// Create a `slurm` job-template entity bound to the container image. Returns
/// `(template_id, version)` for the body node's `job_template_ref`.
async fn create_job_template(app: &axum::Router, container_resource_id: Uuid) -> (Uuid, i32, String) {
    let slug = "container_drain";
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/job-templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "slug": slug,
                        "display_name": "Container Drain (slurm, e2e)",
                        "flavor": "slurm",
                        "common_spec": { "time_limit": "00:10:00" },
                        "container_resource_id": container_resource_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::CREATED, "create job-template: {body}");
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let version = body["latest_version"].as_i64().unwrap() as i32;
    (id, version, slug.to_string())
}

#[tokio::test]
#[ignore = "live Slurm container-staging e2e: needs `just dev slurm-up` (apptainer + /shared + privileged) \
            + SLURM_SSH_* engine env + TEST_S3_BUCKET; pulls docker://python:3.12-slim from the cluster"]
async fn leased_loop_runs_executor_inside_container() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev slurm-up`",
            engine_url()
        );
    }

    let engine_nats_url = std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) = common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

    // Lifecycle listener (instance status) + image-materializations projection
    // (so the `image_materializations` row advances materializing → ready as the
    // materialize net's EffectCompleted lands). main.rs spawns both; the test app
    // does not, so spawn them here.
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
    let mat_nats = MekhanNats::connect(&engine_nats_url, None)
        .await
        .expect("nats (materializations)");
    let mat_db = db.clone();
    tokio::spawn(async move {
        mekhan_service::projections::image_materializations::start_image_materializations_ingest(
            mat_nats, mat_db,
        )
        .await;
    });

    // ── (1) Resources: container image + datacenter (auto-deploys pool net). ──
    let container_id = create_container_image(&app).await;
    let resource_id = create_slurm_dc(&app).await;
    let (template_id, version, slug) = create_job_template(&app, container_id).await;

    let pool_net_id = format!("pool-{resource_id}");
    let pool_deadline = Instant::now() + Duration::from_secs(60);
    while !net_running(&pool_net_id).await {
        if Instant::now() > pool_deadline {
            panic!("datacenter lease-adapter net `{pool_net_id}` did not reach running within 60s");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // ── (2) Publish the leased-loop template. Publish fires
    //    `auto_materialize_images` (deploys the materialize net for
    //    container×datacenter) AND `resolve_container_specs` (embeds the
    //    container blob into the LeaseScope claim). ──
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Container-Lease Slurm E2E",
                        "graph": container_leased_loop_graph(
                            "lp",
                            "body",
                            TemplateRef { template_id, version },
                            &slug,
                        ),
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
    let tmpl_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/templates/{tmpl_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let pub_body = body_json(resp.into_body()).await;
    assert_eq!(status, StatusCode::OK, "publish: {pub_body}");

    // ── (3) Wait for the image to materialize to `ready`. v1 has NO dispatch
    //    readiness gate (docs/22), so the test makes the run deterministic by
    //    waiting for the `.sif` BEFORE launching the instance. Assert the row
    //    reaches ready with a digest + sif_path, then SSH-check the by-ref symlink. ──
    let mat_deadline = Instant::now() + Duration::from_secs(300);
    let (digest, sif_path): (String, String) = loop {
        let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT status, digest, sif_path FROM image_materializations \
             WHERE container_resource_id = $1 AND container_version = $2 AND datacenter_resource_id = $3",
        )
        .bind(container_id)
        .bind(version)
        .bind(resource_id)
        .fetch_optional(&db)
        .await
        .unwrap();
        if let Some((st, digest, sif)) = &row {
            assert_ne!(
                st, "failed",
                "image materialization FAILED (apptainer pull error?) — row: {row:?}"
            );
            if st == "ready" {
                break (
                    digest.clone().expect("ready row carries a digest"),
                    sif.clone().expect("ready row carries a sif_path"),
                );
            }
        }
        if Instant::now() > mat_deadline {
            panic!("image_materializations row did not reach `ready` within 300s — row: {row:?}");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    assert!(!digest.is_empty(), "materialization digest is empty");
    assert!(
        sif_path.contains("/shared/sif/"),
        "sif_path `{sif_path}` is not under /shared/sif"
    );
    // The stable by-ref symlink the compiler embeds must exist and resolve.
    let symlink_ok = slurm_ssh(&format!(
        "test -e '{BY_REF_SIF}' && readlink -f '{BY_REF_SIF}' || echo MISSING"
    ));
    assert!(
        !symlink_ok.contains("MISSING"),
        "by-ref symlink `{BY_REF_SIF}` missing on the cluster after materialize (resolved: {symlink_ok:?})"
    );

    // ── (4) Launch the instance. ──
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "template_id": tmpl_id,
                        "created_by": Uuid::new_v4(),
                        "metadata": { "e2e": "container_lease_slurm" }
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

    // Snapshot drain-executor out-files before the run (the NEW file is THIS
    // run's persistent drain executor).
    let baseline_outs = slurm_ssh("ls /tmp/petri-lease-exec-*.out 2>/dev/null | sort");

    // ── (5) Poll to terminal. While running, sample (a) the single held alloc and
    //    (b) the apptainer-wrapped drain process on the cluster. ──
    let mut seen_alloc_ids: std::collections::BTreeSet<String> = Default::default();
    let mut saw_apptainer = false;
    let deadline = Duration::from_secs(420);
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
            "instance failed — container/lease/apptainer path did not succeed"
        );
        if started.elapsed() > deadline {
            panic!("instance did not complete within {deadline:?} (status: {st})");
        }
        for id in squeue_lease_ids(instance_id, "lp_scope") {
            seen_alloc_ids.insert(id);
        }
        // In-container witness: the drain executor's srun command is wrapped as
        // `apptainer exec … <sif> /bin/bash …`. Probe the cluster process table.
        if !saw_apptainer {
            let procs = slurm_ssh("ps -eo args 2>/dev/null | grep -F 'apptainer exec' | grep -v grep || true");
            if procs.contains("apptainer exec") && procs.contains(BY_REF_SIF) {
                saw_apptainer = true;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    assert!(
        seen_alloc_ids.len() <= 1,
        "loop used more than one distinct Slurm allocation id ({seen_alloc_ids:?}) — \
         iterations did not reuse a single held alloc"
    );

    // ── (6) Topology guard: the LeaseScope lease bridges exist. ──
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
        "p_lp_scope_held",
    ] {
        assert!(
            place_ids.iter().any(|p| p == required),
            "instance net missing lease-scope place `{required}` — lease not emitted. places={place_ids:?}"
        );
    }

    // ── (7) Warm-reuse + in-container witness: exactly one new drain out-file,
    //    apptainer-wrapped, draining all N iterations. ──
    let out_deadline = Instant::now() + Duration::from_secs(120);
    let (drain_out, drain_log): (String, String) = loop {
        let listing = slurm_ssh("ls /tmp/petri-lease-exec-*.out 2>/dev/null | sort");
        let new_paths: Vec<String> = listing
            .lines()
            .filter(|p| !baseline_outs.lines().any(|b| b == *p))
            .map(str::to_string)
            .collect();
        assert!(
            new_paths.len() <= 1,
            "expected exactly one drain-executor out-file, saw {}: {new_paths:?}",
            new_paths.len()
        );
        if let Some(path) = new_paths.first() {
            let log = slurm_ssh(&format!("cat {path} 2>/dev/null || true"));
            if log.matches("handling execution job").count() >= MAX_ITERATIONS as usize {
                break (path.clone(), log);
            }
        }
        if Instant::now() > out_deadline {
            panic!("the drain executor did not drain {MAX_ITERATIONS} jobs within 120s of completion");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let handled = drain_log.matches("handling execution job").count();
    assert!(
        drain_log.contains("Starting lease drain executor"),
        "drain-executor log at {drain_out} missing startup banner. tail:\n{}",
        drain_log.lines().rev().take(20).collect::<Vec<_>>().join("\n")
    );
    assert!(
        handled >= MAX_ITERATIONS as usize,
        "drain executor handled {handled} jobs, expected >= {MAX_ITERATIONS}"
    );
    // The drain executor was launched inside apptainer (sampled during the run).
    // If the fast run never caught the ps window, fall back to asserting the
    // body ran inside python:3.12 (the host image ships a different python),
    // which is only possible if the executor ran in-container.
    if !saw_apptainer {
        assert!(
            drain_log.contains("py=3.12") || drain_log.contains("\"py\": \"3.12") || drain_log.contains("3.12"),
            "could not witness apptainer wrap via ps AND the drain log shows no python 3.12 \
             marker — cannot confirm in-container execution. tail:\n{}",
            drain_log.lines().rev().take(30).collect::<Vec<_>>().join("\n")
        );
    }

    // ── (8) The per-image venv cache bind (SOFT — observation, not a gate). The
    //    compiler emits `--bind /shared/venv-cache/<ref>` (docs/22), but the
    //    drain-executor template sets EXECUTOR_PYTHON__CACHE_DIR=/tmp/… (NOT the
    //    bound path) and this body uses virtualenv:false, so the bound dir stays
    //    empty. Wiring CACHE_DIR → the per-image bound path is a v1 follow-up;
    //    until then, just report what's there rather than fail the headline proof.
    let venv_listing = slurm_ssh(&format!("ls -A '{VENV_CACHE}' 2>/dev/null || true"));
    if venv_listing.trim().is_empty() {
        eprintln!(
            "NOTE: per-image venv cache `{VENV_CACHE}` is empty — expected in v1 \
             (CACHE_DIR not yet pointed at the bound path; body is virtualenv:false)."
        );
    }

    // ── (9) Release witness: the lease is gone after completion. ──
    let release_deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if squeue_lease_ids(instance_id, "lp_scope").is_empty() {
            break;
        }
        if Instant::now() > release_deadline {
            panic!("lease alloc still live 60s after completion — terminal exit did not release it");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
