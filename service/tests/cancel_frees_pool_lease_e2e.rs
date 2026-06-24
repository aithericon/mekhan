//! End-to-end coverage for the CANCEL → runner-pool lease release seam: an
//! instance whose `AutomatedStep` is admission-controlled by a seeded-token
//! (`Tokens`) capacity holds a `concurrency_limit_grant` token while it runs;
//! cancelling the instance must RELEASE that token so the pool capacity frees.
//!
//!   Start ─▶ AutomatedStep{ Executor.capacity = limit_pool } ─▶ End
//!              │  (the engine fires the pool net's `t_grant` at claim — a
//!              │   `concurrency_limit_grant` token held on `pool-<capacity_id>`)
//!              ▼
//!         body sleeps (holds the token)
//!              │
//!         DELETE /api/v1/instances/{id}
//!              │  → cancel_instance → petri.terminate_net → the engine's
//!              │    release_held_leases_for_instance fires `t_release`
//!              ▼
//!         the pool net's `t_release` frees the token (released_at set)
//!
//! This is the runner-POOL (in-process Tokens admission) counterpart of the
//! Slurm-lease cancel test `scheduled_lease_slurm_e2e.rs::
//! cancelling_a_leased_instance_releases_the_held_alloc`. Unlike that one it
//! needs NO Vault / SSH / Slurm cluster — a `Tokens` capacity's backing net
//! `pool-<id>` is deployed in-process by the engine, so the grant/release is
//! observable purely from mekhan's API + DB on the dev stack.
//!
//! The held/freed token is observed through the `allocations` projection (the
//! same `concurrency_limit_grant` rows `GET /api/v1/capacities` reads for
//! `live.Tokens.in_use`): the test spawns `start_allocations_ingest` to fold
//! the pool net's `t_grant`/`t_release` `TransitionFired` events into the
//! `allocations` table, then polls held-count (rows with `released_at IS NULL`)
//! on `pool-<capacity_id>` — 1 while the instance holds, back to 0 after cancel.
//!
//! ── Prerequisites ──
//!
//!   just dev up                         # engine, NATS, Postgres, S3
//!   just dev::e2e-partition             # → TEST_WORKER_DEFAULT_PARTITION
//!
//! The dev executor must be enrolled (its partition exported as
//! `TEST_WORKER_DEFAULT_PARTITION`) so the pooled `AutomatedStep`'s job lands on
//! a partition a running worker drains — otherwise the body never starts and the
//! grant is never claimed. Run serially (`--test-threads=1`); `#[ignore]` so the
//! default no-stack `cargo test` lane skips it:
//!
//!   TEST_S3_BUCKET=mekhan-artifacts \
//!   AWS_ENDPOINT_URL=http://localhost:19005 \
//!   AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin \
//!   cargo test -p mekhan-service --test cancel_frees_pool_lease_e2e \
//!       -- --ignored --test-threads=1 --nocapture

mod common;

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
    default_output_port, CapacityBinding, DeploymentModel, ExecutionBackendType,
    ExecutionSpecConfig, Port, Position, WorkflowEdge, WorkflowGraph, WorkflowNode,
    WorkflowNodeData,
};
use mekhan_service::nats::MekhanNats;
use mekhan_service::projections::allocations::start_allocations_ingest;

/// Workspace alias (snake_case) of the seeded-token capacity the step claims
/// against. Created as a `capacity` resource with the `limit` preset (Seeded →
/// `Tokens` backend); publish resolves it to `pool-<resource_id>`.
const POOL_ALIAS: &str = "limit_pool";

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

/// `Start → AutomatedStep{ Executor.capacity = limit_pool } → End`. The single
/// pooled step claims one token from the `limit_pool` Tokens capacity's backing
/// net for the duration of its (sleeping) body — exactly the
/// claim/register/release handshake the compiler wraps an
/// `Executor { capacity: Some }` step in (docs/14).
fn pooled_step_graph(step_id: &str) -> WorkflowGraph {
    WorkflowGraph {
        nodes: vec![
            start("s"),
            WorkflowNode {
                id: step_id.to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: pos(),
                data: WorkflowNodeData::AutomatedStep {
                    label: "Run Python (pooled on limit_pool)".to_string(),
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
                    deployment_model: DeploymentModel::Executor {
                        capacity: Some(CapacityBinding {
                            alias: POOL_ALIAS.to_string(),
                            request: None,
                        }),
                        group: None,
                    },
                    asset_bindings: Vec::new(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            end("e"),
        ],
        edges: vec![
            WorkflowEdge {
                id: "e_in".to_string(),
                source: "s".to_string(),
                target: step_id.to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                join: None,
                edge_type: "sequence".to_string(),
            },
            WorkflowEdge {
                id: "e_out".to_string(),
                source: step_id.to_string(),
                target: "e".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                join: None,
                edge_type: "sequence".to_string(),
            },
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    }
}

/// The pooled body SLEEPS so the held token stays held long enough for us to
/// observe it and then cancel. It releases on the loop/step's terminal exit —
/// but here cancel pre-empts that, and the engine's
/// `release_held_leases_for_instance` is what must free the token.
const SLEEP_PY: &str = r#"import time
log_info("pooled body sleeping to hold the concurrency token")
time.sleep(45)
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

/// Count of HELD `concurrency_limit_grant` rows on the capacity's backing pool
/// net — the same predicate `GET /api/v1/capacities` uses for `live.Tokens
/// .in_use` (`tokens_live`: `released_at IS NULL` on `pool-<resource_id>`).
async fn held_pool_grants(db: &sqlx::PgPool, capacity_id: Uuid) -> i64 {
    let net_id = format!("pool-{capacity_id}");
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM allocations \
         WHERE net_id = $1 AND kind = 'concurrency_limit_grant' \
           AND released_at IS NULL",
    )
    .bind(&net_id)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn instance_status(db: &sqlx::PgPool, instance_id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM workflow_instances WHERE id = $1")
        .bind(instance_id)
        .fetch_one(db)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "live runner-pool cancel e2e: needs `just dev up` (engine, NATS, Postgres, S3) + \
            the dev executor enrolled with TEST_WORKER_DEFAULT_PARTITION (just dev::e2e-partition) \
            so the pooled step's job is drained and holds the token. No Vault/SSH/Slurm needed."]
async fn cancelling_instance_with_runner_pool_lease_frees_capacity() {
    if !engine_available().await {
        panic!(
            "engine not available at {} — start the stack with `just dev up`",
            engine_url()
        );
    }

    let engine_nats_url = std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let (app, db) = common::test_app_with_petri_url(&engine_nats_url, &engine_url()).await;

    // Lifecycle listener: drives instance status → terminal in the DB (so the
    // cancel's `cancelled` row + any completion is reflected) exactly as the
    // Slurm-lease cancel test does.
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
        db.clone(),
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

    // Allocations projection: folds the pool net's `t_grant`/`t_release`
    // `TransitionFired` events into the `allocations` table so we can read the
    // held-token count (the `concurrency_limit_grant` rows). A per-process
    // consumer prefix keeps this off the production `mekhan-allocations-v2`
    // durable owned by the live dev daemon (test-prefixed durables start at
    // `DeliverPolicy::New`).
    {
        let alloc_nats = MekhanNats::connect(&engine_nats_url, None)
            .await
            .expect("nats")
            .with_consumer_prefix(format!("test_cancel_pool_{}", Uuid::new_v4().simple()));
        let alloc_db = db.clone();
        tokio::spawn(async move {
            start_allocations_ingest(alloc_nats, alloc_db).await;
        });
    }

    // ── (1) Create the seeded-token capacity. `create_resource` auto-deploys its
    //    backing token-pool net `pool-<resource_id>` (the step's
    //    claim/register/release bridges target it). The `limit` preset locks the
    //    Seeded axes (→ `Tokens` backend); `capacity_amount` sets the seeded
    //    count (1 unit is enough — one step, one token).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/resources")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "path": POOL_ALIAS,
                        "resource_type": "capacity",
                        "display_name": "Concurrency Limit (cancel e2e)",
                        "config": {
                            "preset": "limit",
                            "capacity_amount": 1
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let cap_status = resp.status();
    let cap_body = body_json(resp.into_body()).await;
    assert_eq!(cap_status, StatusCode::CREATED, "create capacity: {cap_body}");
    let capacity_id: Uuid = cap_body["id"].as_str().unwrap().parse().unwrap();

    // Wait for the auto-deployed token-pool net to reach running before
    // publishing/launching the pooled template.
    let pool_net_id = format!("pool-{capacity_id}");
    let pool_deadline = Instant::now() + Duration::from_secs(60);
    while !net_running(&pool_net_id).await {
        if Instant::now() > pool_deadline {
            panic!(
                "token-pool net `{pool_net_id}` did not reach running within 60s \
                 — ensure_token_pool_net_deployed may have failed (engine reachable?)"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // Sanity: no token is held before any instance exists.
    assert_eq!(
        held_pool_grants(&db, capacity_id).await,
        0,
        "pool should have zero held grants before launch"
    );

    // ── (2) Build + publish the pooled-step template. The `files` key maps to
    //    the step node id (`step`), staging its sleeping `main.py`.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Pooled-Step Cancel E2E",
                        "graph": pooled_step_graph("step"),
                        "files": { "step": { "main.py": SLEEP_PY } },
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
                        "metadata": { "e2e": "cancel_frees_pool_lease" }
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

    // ── (4) Wait for the pool token to be CLAIMED — the step's claim fires the
    //    pool net's `t_grant`, projected as a held `concurrency_limit_grant` row
    //    on `pool-<capacity_id>`. (`in_use` >= 1.)
    let acquire_deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if held_pool_grants(&db, capacity_id).await >= 1 {
            break;
        }
        let st = instance_status(&db, instance_id).await;
        assert!(
            st != "failed" && st != "completed" && st != "cancelled",
            "instance reached {st} before the pool token was ever observed held \
             — the pooled step never claimed (worker draining the partition?)"
        );
        if Instant::now() > acquire_deadline {
            panic!(
                "pool token never claimed within 120s — the `Executor.capacity` step \
                 did not acquire a `concurrency_limit_grant` on {pool_net_id} \
                 (is the dev executor enrolled on TEST_WORKER_DEFAULT_PARTITION?)"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // ── (5) CANCEL the instance while it holds the token.
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
    assert_eq!(cancelled["status"], "cancelled", "instance marked cancelled");

    // ── (6) Assert the pool capacity FREES — the held `concurrency_limit_grant`
    //    drops back to 0. `cancel_instance` → `petri.terminate_net` → the
    //    engine's `release_held_leases_for_instance` fires the pool net's
    //    `t_release` (released_at set). Runner pools are in-process, so this is
    //    fast (no cluster lag) — a generous 30s tolerates projection latency.
    let release_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let held = held_pool_grants(&db, capacity_id).await;
        if held == 0 {
            break;
        }
        if Instant::now() > release_deadline {
            panic!(
                "pool capacity not freed within 30s of cancel — {held} held \
                 `concurrency_limit_grant` row(s) remain on {pool_net_id}; \
                 release_held_leases_for_instance may not have fired (LEAKED token)"
            );
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // ── (7) Corroborating witness via the public capacity API: `GET
    //    /api/v1/capacities` reads the SAME held-grant predicate for
    //    `live.Tokens.in_use`, so the freed pool must report `in_use == 0`.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v1/capacities")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "list capacities");
    let caps = body_json(resp.into_body()).await;
    let cap = caps
        .as_array()
        .expect("capacities is an array")
        .iter()
        .find(|c| c["id"].as_str() == Some(capacity_id.to_string().as_str()))
        .unwrap_or_else(|| panic!("capacity {capacity_id} not in /api/v1/capacities: {caps}"));
    let in_use = cap["live"]["in_use"].as_u64().unwrap_or(0);
    assert_eq!(
        in_use, 0,
        "GET /api/v1/capacities reports live.in_use={in_use} for the freed pool \
         — the cancel did not release the held token. live={}",
        cap["live"]
    );
}
