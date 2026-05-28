//! End-to-end test for the full ADR-18 causality pipeline.
//!
//! Exercises: SDK scenario → petri-lab engine → executor (Python backend) →
//! catalogue artifacts → Mekhan causality consumer → provenance queries →
//! human task lifecycle → process tracking.
//!
//! Requires the full local stack running:
//!   just dev up   # Postgres + NATS + S3 + executor + engine + mekhan
//!
//! The engine/executor connect to the dev NATS broker (`docker-compose.yml`
//! maps `4333:4222`); the test harness defaults to that same broker.
//!
//! Run with:
//!   cargo test --test causality_e2e -- --test-threads=1 --nocapture
//!
//! Override the broker via `ENGINE_NATS_URL` only if the engine was started
//! against a non-default NATS.

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::causality::ingest::start_causality_ingest;
use mekhan_service::causality::live::LiveBroadcasts;
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::nats::MekhanNats;

// ── Helpers ────────────────────────────────────────────────────────────────

fn engine_nats_url() -> String {
    std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url())
}

fn engine_url() -> String {
    std::env::var("TEST_ENGINE_URL").unwrap_or_else(|_| "http://localhost:3030".to_string())
}

async fn engine_available() -> bool {
    reqwest::get(format!("{}/api/nets/metadata", engine_url()))
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Compile the SDK example to AIR JSON.
///
/// The example is part of the in-repo `aithericon-sdk` workspace at
/// `../engine` (relative to the `service/` crate root, which is cargo's CWD
/// when running tests).
fn compile_sdk_example(name: &str) -> Value {
    let output = std::process::Command::new("cargo")
        .args(["run", "-p", "aithericon-sdk", "--example", name])
        .current_dir("../engine")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("failed to run SDK example");

    assert!(
        output.status.success(),
        "SDK example failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).expect("invalid AIR JSON from SDK example")
}

/// Deploy a scenario via HTTP API and start it.
async fn deploy_scenario(net_id: &str, air_json: &Value) {
    let client = reqwest::Client::new();
    let base = engine_url();

    // Deploy scenario
    let resp = client
        .post(format!("{base}/api/nets/{net_id}/scenario"))
        .json(air_json)
        .send()
        .await
        .expect("deploy scenario");
    assert!(
        resp.status().is_success(),
        "deploy failed: {} - {}",
        resp.status(),
        resp.text().await.unwrap_or_default()
    );

    // Set run mode to running
    let resp = client
        .put(format!("{base}/api/nets/{net_id}/run-mode"))
        .json(&json!({ "mode": "running" }))
        .send()
        .await
        .expect("set run mode");
    assert!(
        resp.status().is_success(),
        "set run mode failed: {}",
        resp.status()
    );
}

/// Insert a workflow_instances row so the lifecycle listener can update it.
async fn insert_instance(db: &sqlx::PgPool, instance_id: Uuid, net_id: &str) {
    let template_id = Uuid::new_v4();
    let author_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO workflow_templates (id, name, graph, is_latest, published, author_id) \
         VALUES ($1, 'e2e-test', '{}'::jsonb, true, true, $2)",
    )
    .bind(template_id)
    .bind(author_id)
    .execute(db)
    .await
    .expect("insert template");

    sqlx::query(
        r#"INSERT INTO workflow_instances
           (id, template_id, template_version, net_id, status, created_by, started_at, metadata)
           VALUES ($1, $2, 1, $3, 'running', $4, NOW(), '{}')"#,
    )
    .bind(instance_id)
    .bind(template_id)
    .bind(net_id)
    .bind(author_id)
    .execute(db)
    .await
    .expect("insert running instance");
}

/// Poll for instance status change.
async fn wait_for_instance_status(
    db: &sqlx::PgPool,
    instance_id: Uuid,
    target: &str,
    timeout: Duration,
) {
    let start = std::time::Instant::now();
    loop {
        let status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM workflow_instances WHERE id = $1",
        )
        .bind(instance_id)
        .fetch_optional(db)
        .await
        .unwrap()
        .flatten();

        if status.as_deref() == Some(target) {
            return;
        }
        if start.elapsed() > timeout {
            panic!(
                "instance {instance_id} did not reach '{target}' within {timeout:?} (current: {status:?})"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Poll for a human task to appear for a given net_id.
async fn wait_for_task_by_net(
    db: &sqlx::PgPool,
    net_id: &str,
    timeout: Duration,
) -> (String, String) {
    let start = std::time::Instant::now();
    loop {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT id, process_id FROM hpi_tasks WHERE detail->>'net_id' = $1 LIMIT 1",
        )
        .bind(net_id)
        .fetch_optional(db)
        .await
        .unwrap();

        if let Some(r) = row {
            return r;
        }
        if start.elapsed() > timeout {
            panic!("no hpi_task appeared for net_id={net_id} within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Poll for causality events to appear for a net.
async fn wait_for_causality_events(
    db: &sqlx::PgPool,
    net_id: &str,
    min_count: i64,
    timeout: Duration,
) {
    let start = std::time::Instant::now();
    loop {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM causality_events WHERE net_id = $1",
        )
        .bind(net_id)
        .fetch_one(db)
        .await
        .unwrap_or(0);

        if count >= min_count {
            return;
        }
        if start.elapsed() > timeout {
            panic!(
                "expected ≥{min_count} causality_events for {net_id}, got {count} within {timeout:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Abort-on-drop handle for spawned tasks.
struct TaskHandle(tokio::task::AbortHandle);
impl Drop for TaskHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn spawn_consumer<F, Fut>(f: F) -> TaskHandle
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let handle = tokio::spawn(f());
    tokio::time::sleep(Duration::from_millis(200)).await;
    TaskHandle(handle.abort_handle())
}

// ── Test ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn causality_full_pipeline() {
    // Init tracing for debug output
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mekhan_service=info".into()),
        )
        .try_init();

    // ── 1. Prerequisites ─────────────────────────────────────────────────
    if !engine_available().await {
        eprintln!(
            "SKIP: engine not available at http://localhost:3030\n\
             Start the full local stack with: just dev up"
        );
        return;
    }

    let nats_url = engine_nats_url();
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("connect to NATS")
        .with_consumer_prefix(format!("test_{}", Uuid::new_v4().simple()));

    // Build router using the SAME db pool as our consumers
    // (test_app_with_nats creates a separate DB which wouldn't see causality data)
    let config = common::test_config();
    let petri = mekhan_service::petri::client::PetriClient::new(&config.petri_lab_url);
    let yjs_persistence = mekhan_service::yjs::persistence::YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(mekhan_service::yjs::manager::YjsManager::new(yjs_persistence));
    let artifact_store = Arc::new(mekhan_service::s3::ArtifactStore::new(&config.s3));
    let catalogue_repo = Arc::new(mekhan_service::catalogue::repository::PgCatalogueRepository::new(db.clone()));
    let (token_verifier, principal_resolver) = common::default_test_auth();
    let session_store: Arc<dyn mekhan_service::auth::bff::session::SessionStore> =
        Arc::new(mekhan_service::auth::bff::session::PgSessionStore::new(db.clone()));
    let triggers = Arc::new(mekhan_service::triggers::TriggerDispatcher::new(
        db.clone(),
        petri.clone(),
        nats.clone(),
    ));
    let app = mekhan_service::build_router(mekhan_service::AppState {
        db: db.clone(),
        petri,
        nats: nats.clone(),
        config,
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3: None,
        catalogue_repo,
        live: LiveBroadcasts::new(),
        authenticator: Arc::new(
            mekhan_service::auth::authenticator::NoopAuthenticator::default(),
        ),
        session_store,
        oidc: None,
        token_verifier,
        principal_resolver,
        introspection: None,
        zitadel_mgmt: None,
        triggers,
        result_waiters: mekhan_service::triggers::ResultWaiters::new(),
        resource_store: Arc::new(aithericon_resources::InMemoryResourceStore::new()),
        resource_resolver: Arc::new(
            mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()),
        ),
    });

    // ── 2. Spawn Mekhan consumers ────────────────────────────────────────
    //
    // The `MekhanNats` above carries a per-test consumer prefix, so the
    // durables we create here (`{prefix}_mekhan-lifecycle`,
    // `{prefix}_mekhan-causality-ingest`) are unique to this test run and
    // start at `DeliverPolicy::New`. No purge of shared streams — that
    // would destroy the live dev daemon's in-flight state.

    // Subscription manager (needed by both causality ingest and lifecycle listener)
    let kv = nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("create KV");
    let sub_mgr = Arc::new(SubscriptionManager::new(
        kv,
        nats.jetstream().clone(),
    ));

    // Single consumer projects catalogue, human tasks, and step/metric/log
    // breadcrumbs off the petri.events.> stream.
    let c_nats = nats.clone();
    let c_db = db.clone();
    let c_sub = sub_mgr.clone();
    let c_live = LiveBroadcasts::new();
    let _causality = spawn_consumer(move || start_causality_ingest(c_nats, c_db, c_sub, c_live, None)).await;

    // Lifecycle listener
    let l_nats = nats.clone();
    let l_db = db.clone();
    let l_sub = sub_mgr.clone();
    let _lifecycle =
        spawn_consumer(move || start_lifecycle_listener(l_nats, l_db, l_sub, None, mekhan_service::triggers::ResultWaiters::new())).await;

    // ── 3. Compile & deploy scenario ─────────────────────────────────────

    let air_json = compile_sdk_example("causality_e2e_net");
    let net_id = format!("mekhan-{}", Uuid::new_v4().simple());
    let instance_id = Uuid::new_v4();

    // Insert DB row BEFORE deployment (lifecycle listener needs it)
    insert_instance(&db, instance_id, &net_id).await;

    // Deploy via HTTP API
    deploy_scenario(&net_id, &air_json).await;
    eprintln!("  deployed net: {net_id}");

    // ── 4. Wait for human task ───────────────────────────────────────────
    //
    // The net will: seed → t_prepare → t_request_review (human_task effect)
    // This publishes to HUMAN_REQUESTS → our task_ingest consumer picks it up.

    let (task_id, task_process_id) =
        wait_for_task_by_net(&db, &net_id, Duration::from_secs(15)).await;
    eprintln!("  human task appeared: {task_id} (process: {task_process_id})");

    // Verify task detail
    let task_detail: Value = sqlx::query_scalar(
        "SELECT detail FROM hpi_tasks WHERE id = $1",
    )
    .bind(&task_id)
    .fetch_one(&db)
    .await
    .expect("fetch task detail");
    assert_eq!(
        task_detail["net_id"].as_str(),
        Some(net_id.as_str()),
        "task should reference our net"
    );

    // ── 5. Complete the human task ───────────────────────────────────────
    //
    // Publish human completion signal. The engine's GlobalHumanResultListener
    // picks this up and injects a token into sig_review.

    let review_place = task_detail["place"]
        .as_str()
        .expect("task detail should have place");

    let completion = json!({
        "task_id": task_id,
        "data": { "approved": "yes" },
        "completed_at": chrono::Utc::now().to_rfc3339()
    });
    let subject = format!("human.completed.{net_id}.{review_place}");
    nats.client()
        .publish(subject.clone(), serde_json::to_vec(&completion).unwrap().into())
        .await
        .expect("publish human completion");
    eprintln!("  published human completion to {subject}");

    // ── 6. Wait for net completion ───────────────────────────────────────
    //
    // After approval, the net dispatches to executor (Python backend).
    // The executor runs the script, emits artifact, completes.
    // NetCompleted fires → lifecycle listener updates instance status.

    wait_for_instance_status(
        &db,
        instance_id,
        "completed",
        Duration::from_secs(90), // venv setup can be slow
    )
    .await;
    eprintln!("  instance completed");

    // ── 7. Assert causality ──────────────────────────────────────────────

    // Wait for causality events to accumulate (at least 3: TokenCreated + TransitionFired + EffectCompleted)
    wait_for_causality_events(&db, &net_id, 3, Duration::from_secs(10)).await;

    let event_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM causality_events WHERE net_id = $1",
    )
    .bind(&net_id)
    .fetch_one(&db)
    .await
    .unwrap();
    eprintln!("  causality events: {event_count}");
    assert!(event_count >= 3, "expected ≥3 causality events, got {event_count}");

    // Check token roles exist
    let token_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM causality_event_tokens WHERE net_id = $1",
    )
    .bind(&net_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(token_count > 0, "expected causality_event_tokens rows");

    // Check process tags (seed tokens should self-tag)
    let tag_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM causality_process_tags pt \
         JOIN causality_event_tokens et ON et.token_id = pt.token_id \
         WHERE et.net_id = $1",
    )
    .bind(&net_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(tag_count > 0, "expected process tags for this net's tokens");

    // Check hpi_processes auto-created
    let process_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM hpi_processes hp \
         JOIN causality_process_tags pt ON pt.process_id = hp.process_id \
         JOIN causality_event_tokens et ON et.token_id = pt.token_id \
         WHERE et.net_id = $1",
    )
    .bind(&net_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(
        process_count > 0,
        "expected auto-created hpi_processes for this net"
    );

    // ── 8. Assert provenance API ─────────────────────────────────────────

    // Find a produced token from a TransitionFired event (guaranteed to have ancestors)
    let some_token: String = sqlx::query_scalar(
        "SELECT et.token_id FROM causality_event_tokens et \
         JOIN causality_events ce ON ce.net_id = et.net_id AND ce.event_seq = et.event_seq \
         WHERE et.net_id = $1 AND et.role = 'produced' AND ce.event_type = 'TransitionFired' \
         LIMIT 1",
    )
    .bind(&net_id)
    .fetch_one(&db)
    .await
    .expect("should have at least one token produced by a transition");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/api/v1/provenance/{net_id}/{some_token}?depth=5"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let provenance = body_json(resp.into_body()).await;
    let nodes = provenance["nodes"]
        .as_array()
        .expect("provenance.nodes should be array");
    eprintln!("  provenance nodes for {some_token}: {}", nodes.len());
    // At minimum depth 0 (the event that produced this token)
    assert!(!nodes.is_empty(), "provenance should return at least 1 node");

    // ── 9. Assert catalogue (if executor completed with artifact) ────────

    // Give catalogue ingest a moment to process
    tokio::time::sleep(Duration::from_secs(2)).await;

    let catalogue_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM catalogue_entries WHERE source_net = $1",
    )
    .bind(&net_id)
    .fetch_one(&db)
    .await
    .unwrap_or(0);
    eprintln!("  catalogue entries for net: {catalogue_count}");
    // Note: catalogue_count may be 0 if the catalogue ingest consumer isn't running
    // in this test. The key assertion is that causality + tasks + lifecycle work.

    eprintln!("  ✓ causality_full_pipeline passed");
}

/// Build a unique consumer prefix for this test invocation. With it set
/// on `MekhanNats`, the lifecycle + causality durables are uniquely named
/// so parallel runs (and the live dev daemon) keep independent cursors
/// on the shared streams. Replaces the old `clean_slate` purge.
fn test_prefix() -> String {
    format!("test_{}", Uuid::new_v4().simple())
}

/// Runtime proof of the `{{ <slug>.<field> }}` human-task interpolation:
///
/// Start declares a `file` start-param (`invoice_file`); the Review task's
/// image + download blocks and its instructions reference it via
/// `{{ invoice_file.url }}` / `{{ invoice_id }}`. Going through the real
/// template → publish → `parameterize_air` (seeds the start token) → engine
/// (evaluates the injected Rhai) → causality consumer (`to_human_task_json`)
/// path, the projected task must carry the *resolved* values, never the raw
/// placeholder.
#[tokio::test]
async fn interpolated_human_task_resolves_start_file_param() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mekhan_service=info".into()),
        )
        .try_init();

    if !engine_available().await {
        eprintln!(
            "SKIP: engine not available at http://localhost:3030\n\
             Start the full local stack with: just dev up"
        );
        return;
    }

    let engine_nats = engine_nats_url();
    let (app, db) = common::test_app_with_petri_url(&engine_nats, &engine_url()).await;

    let nats = MekhanNats::connect(&engine_nats, None)
        .await
        .expect("connect to NATS")
        .with_consumer_prefix(test_prefix());

    let kv = nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("create KV");
    let sub_mgr = Arc::new(SubscriptionManager::new(kv, nats.jetstream().clone()));

    let c_nats = nats.clone();
    let c_db = db.clone();
    let c_sub = sub_mgr.clone();
    let c_live = LiveBroadcasts::new();
    let _causality =
        spawn_consumer(move || start_causality_ingest(c_nats, c_db, c_sub, c_live, None)).await;

    let l_nats = nats.clone();
    let l_db = db.clone();
    let l_sub = sub_mgr.clone();
    let _lifecycle =
        spawn_consumer(move || start_lifecycle_listener(l_nats, l_db, l_sub, None, mekhan_service::triggers::ResultWaiters::new())).await;

    // Minimal Start(file param) → Review(interpolated blocks) → End graph.
    let graph = json!({
        "nodes": [
            {
                "id": "start",
                "type": "start",
                "position": { "x": 0, "y": 0 },
                "data": {
                    "type": "start",
                    "label": "Start",
                    "initial": {
                        "id": "in",
                        "label": "Invoice Intake",
                        "fields": [
                            { "name": "invoice_file", "label": "Invoice Document",
                              "kind": "file", "required": true },
                            { "name": "invoice_id", "label": "Invoice ID",
                              "kind": "text", "required": false }
                        ]
                    }
                }
            },
            {
                "id": "review",
                "type": "human_task",
                "position": { "x": 240, "y": 0 },
                "data": {
                    "type": "human_task",
                    "label": "Review Invoice",
                    "taskTitle": "Review {{ invoice_id }}",
                    "instructionsMdsvex": "Verify invoice {{ invoice_id }} below.",
                    "steps": [
                        {
                            "id": "step-verify",
                            "title": "Verify Details",
                            "blocks": [
                                { "type": "image", "url": "{{ invoice_file.url }}",
                                  "alt": "Uploaded invoice",
                                  "caption": "Original invoice" },
                                { "type": "download", "downloads": [
                                    { "url": "{{ invoice_file.url }}",
                                      "filename": "{{ invoice_file.filename }}",
                                      "mime_type": "{{ invoice_file.content_type }}",
                                      "description": "Original uploaded invoice" }
                                ] },
                                { "type": "input", "field": {
                                    "name": "approved", "label": "Approved",
                                    "kind": "checkbox", "required": true } }
                            ]
                        }
                    ]
                }
            },
            {
                "id": "end",
                "type": "end",
                "position": { "x": 480, "y": 0 },
                "data": { "type": "end", "label": "Done" }
            }
        ],
        "edges": [
            { "id": "e-start-review", "source": "start", "target": "review",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e-review-end", "source": "review", "target": "end",
              "targetHandle": "in", "type": "sequence" }
        ]
    });

    // 1. Create template
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "name": "Interpolation E2E",
                        "graph": graph,
                        "author_id": Uuid::new_v4(),
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create template");
    let template = body_json(resp.into_body()).await;
    let template_id = template["id"].as_str().unwrap().to_string();

    // 2. Publish (compiles graph → AIR; interpolation is baked into injection)
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
    assert_eq!(resp.status(), StatusCode::OK, "publish template");

    // 3. Create instance with a file start-param (parameterize_air seeds it)
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "template_id": template_id,
                        "created_by": Uuid::new_v4(),
                        "metadata": {},
                        "start_tokens": [{
                            "start_block_id": "start",
                            "token": {
                                "invoice_id": "RE42",
                                "invoice_file": {
                                    "key": "templates/x/v1/start/inv-RE42.pdf",
                                    "url": "https://example.test/inv-RE42.pdf",
                                    "filename": "inv-RE42.pdf",
                                    "content_type": "application/pdf",
                                    "size": 1234
                                }
                            }
                        }]
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create instance");
    let instance = body_json(resp.into_body()).await;
    let net_id = instance["net_id"].as_str().expect("net_id").to_string();
    assert_eq!(instance["status"], "running");

    // 4. Engine runs Start→Review; HumanTaskRequest → HUMAN_REQUESTS →
    //    causality consumer projects the rendered task into hpi_tasks.
    let (task_id, _process_id) =
        wait_for_task_by_net(&db, &net_id, Duration::from_secs(20)).await;

    let (detail, title): (Value, String) =
        sqlx::query_as("SELECT detail, title FROM hpi_tasks WHERE id = $1")
            .bind(&task_id)
            .fetch_one(&db)
            .await
            .expect("fetch task detail");
    let detail_str = serde_json::to_string(&detail).unwrap();
    eprintln!("  task title: {title}\n  task detail: {detail_str}");

    // Placeholders were resolved at runtime against the seeded start token.
    assert!(
        detail_str.contains("https://example.test/inv-RE42.pdf"),
        "image/download url not resolved from start token: {detail_str}"
    );
    assert!(
        detail_str.contains("inv-RE42.pdf"),
        "download filename not resolved: {detail_str}"
    );
    assert!(
        !detail_str.contains("{{"),
        "raw placeholder leaked into rendered task: {detail_str}"
    );
    // Title/instructions interpolation resolved too.
    assert_eq!(title, "Review RE42", "task title not interpolated");
    assert!(
        detail_str.contains("Verify invoice RE42 below."),
        "instructions not interpolated: {detail_str}"
    );

    eprintln!("  ✓ interpolated_human_task_resolves_start_file_param passed");
}

// ── Rich return values: result-envelope pipeline ─────────────────────────
//
// End-to-end proof of Part A: compiler stamps `exit_code` → engine lifts it
// into `NetCompleted` → lifecycle consumer persists it to
// `workflow_instances.result`. Covers the success envelope, the Failure→End
// precedence guard, and the bare-End backward-compat (result stays NULL).

/// Spin up the app + NATS consumers (causality + lifecycle), publish `graph`,
/// create one instance seeded with `start_token`, and return
/// `(app, db, instance_id)` once it is `running`.
async fn rrv_publish_and_create(
    graph: Value,
    start_token: Value,
) -> (axum::Router, sqlx::PgPool, Uuid, TaskHandle, TaskHandle) {
    let engine_nats = engine_nats_url();
    let (app, db) = common::test_app_with_petri_url(&engine_nats, &engine_url()).await;
    let nats = MekhanNats::connect(&engine_nats, None)
        .await
        .expect("connect to NATS")
        .with_consumer_prefix(test_prefix());
    let kv = nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("create KV");
    let sub_mgr = Arc::new(SubscriptionManager::new(kv, nats.jetstream().clone()));

    let c_nats = nats.clone();
    let c_db = db.clone();
    let c_sub = sub_mgr.clone();
    let c_live = LiveBroadcasts::new();
    let causality =
        spawn_consumer(move || start_causality_ingest(c_nats, c_db, c_sub, c_live, None)).await;
    let l_nats = nats.clone();
    let l_db = db.clone();
    let l_sub = sub_mgr.clone();
    let lifecycle = spawn_consumer(move || {
        start_lifecycle_listener(
            l_nats,
            l_db,
            l_sub,
            None,
            mekhan_service::triggers::ResultWaiters::new(),
        )
    })
    .await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "name": "Rich Return E2E",
                        "graph": graph,
                        "author_id": Uuid::new_v4(),
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create template");
    let template_id = body_json(resp.into_body()).await["id"]
        .as_str()
        .unwrap()
        .to_string();

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
    assert_eq!(resp.status(), StatusCode::OK, "publish template");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/instances")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_string(&json!({
                        "template_id": template_id,
                        "created_by": Uuid::new_v4(),
                        "metadata": {},
                        "start_tokens": [{ "start_block_id": "start", "token": start_token }],
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "create instance");
    let instance = body_json(resp.into_body()).await;
    let id = Uuid::parse_str(instance["id"].as_str().unwrap()).unwrap();
    // Hand the consumer handles back — `TaskHandle` aborts on drop, so the
    // caller must keep them alive for the duration of the test.
    (app, db, id, causality, lifecycle)
}

async fn fetch_result(db: &sqlx::PgPool, id: Uuid) -> Option<Value> {
    sqlx::query_scalar::<_, Option<Value>>(
        "SELECT result FROM workflow_instances WHERE id = $1",
    )
    .bind(id)
    .fetch_one(db)
    .await
    .unwrap()
}

fn start_with_fields(fields: Value) -> Value {
    json!({
        "id": "start", "type": "start", "position": { "x": 0, "y": 0 },
        "data": { "type": "start", "label": "Start",
                  "initial": { "id": "in", "label": "In", "fields": fields } }
    })
}

#[tokio::test]
async fn rrv_end_result_mapping_success_envelope() {
    if !engine_available().await {
        eprintln!("SKIP: engine not available — just dev up");
        return;
    }
    let graph = json!({
        "nodes": [
            start_with_fields(json!([
                { "name": "amount", "label": "Amount", "kind": "number", "required": true }
            ])),
            { "id": "end", "type": "end", "position": { "x": 240, "y": 0 },
              "data": { "type": "end", "label": "Done",
                        "resultMapping": [
                            { "targetField": "total", "expression": "input.amount" }
                        ] } }
        ],
        "edges": [
            { "id": "e1", "source": "start", "target": "end",
              "targetHandle": "in", "type": "sequence" }
        ]
    });
    let (app, db, id, _c, _l) =
        rrv_publish_and_create(graph, json!({ "amount": 42 })).await;
    wait_for_instance_status(&db, id, "completed", Duration::from_secs(30)).await;

    assert_eq!(
        fetch_result(&db, id).await,
        Some(json!({ "ok": true, "value": { "total": 42 } })),
        "success envelope persisted from exit_code"
    );

    // The GET handler surfaces it too (model + serde wiring).
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/instances/{id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["result"], json!({ "ok": true, "value": { "total": 42 } }));
    eprintln!("  ✓ rrv_end_result_mapping_success_envelope passed");
}

#[tokio::test]
async fn rrv_failure_precedence_over_end() {
    if !engine_available().await {
        eprintln!("SKIP: engine not available — just dev up");
        return;
    }
    // Start → Failure → End. The Failure stamps the error envelope; the End
    // also declares a resultMapping. The precedence guard must keep the
    // Failure envelope (net still completes — Failure forwards to End).
    let graph = json!({
        "nodes": [
            start_with_fields(json!([
                { "name": "code", "label": "Code", "kind": "text", "required": true }
            ])),
            { "id": "f", "type": "failure", "position": { "x": 240, "y": 0 },
              "data": { "type": "failure", "label": "Fail",
                        "failureMessage": "boom",
                        "errorResultMapping": [
                            { "targetField": "code", "expression": "input.code" }
                        ] } },
            { "id": "end", "type": "end", "position": { "x": 480, "y": 0 },
              "data": { "type": "end", "label": "Done",
                        "resultMapping": [
                            { "targetField": "clobbered", "expression": "\"NOPE\"" }
                        ] } }
        ],
        "edges": [
            { "id": "e1", "source": "start", "target": "f",
              "targetHandle": "in", "type": "sequence" },
            { "id": "e2", "source": "f", "target": "end",
              "targetHandle": "in", "type": "sequence" }
        ]
    });
    let (_app, db, id, _c, _l) =
        rrv_publish_and_create(graph, json!({ "code": "E_BAD" })).await;
    // Failure does NOT change net status — it forwards to End → completed.
    wait_for_instance_status(&db, id, "completed", Duration::from_secs(30)).await;

    let result = fetch_result(&db, id).await.expect("result persisted");
    assert_eq!(result["ok"], json!(false), "Failure envelope preserved");
    assert_eq!(result["error"]["reason"], json!("boom"));
    assert_eq!(result["error"]["value"], json!({ "code": "E_BAD" }));
    assert!(
        !result.to_string().contains("NOPE"),
        "End must not clobber the Failure envelope: {result}"
    );
    eprintln!("  ✓ rrv_failure_precedence_over_end passed");
}

#[tokio::test]
async fn rrv_bare_end_result_is_null() {
    if !engine_available().await {
        eprintln!("SKIP: engine not available — just dev up");
        return;
    }
    // Start → End, no resultMapping, no Failure: backward-compat contract —
    // no exit_code stamped → NetCompleted.exit_code None → result stays NULL.
    let graph = json!({
        "nodes": [
            start_with_fields(json!([
                { "name": "x", "label": "X", "kind": "number", "required": false }
            ])),
            { "id": "end", "type": "end", "position": { "x": 240, "y": 0 },
              "data": { "type": "end", "label": "Done" } }
        ],
        "edges": [
            { "id": "e1", "source": "start", "target": "end",
              "targetHandle": "in", "type": "sequence" }
        ]
    });
    let (_app, db, id, _c, _l) =
        rrv_publish_and_create(graph, json!({ "x": 1 })).await;
    wait_for_instance_status(&db, id, "completed", Duration::from_secs(30)).await;
    assert_eq!(
        fetch_result(&db, id).await,
        None,
        "bare End must leave result NULL (unchanged legacy behavior)"
    );
    eprintln!("  ✓ rrv_bare_end_result_is_null passed");
}
