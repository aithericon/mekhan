//! Reusable test server for integration tests.
//!
//! Spins up a real in-memory petri-lab engine HTTP server on a random port.
//! Supports both single-net mode (basic) and multi-net registry mode.
//! No NATS, no Docker — just axum + in-memory stores.
//!
//! # Usage
//!
//! ```ignore
//! // Multi-net mode (recommended — matches production)
//! let server = TestServer::multi_net().await;
//! server.deploy_net("my-net", &scenario).await;
//! server.evaluate_net("my-net", 10).await;
//! let state = server.get("/api/nets/my-net/state").await;
//! ```

use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::{broadcast, Notify};

use petri_api::dto::RunMode;
use petri_api::net_registry::{NetRegistry, StoreFactory};
use petri_api::router::{create_router, create_router_with_registry, AppState};
use petri_application::{AdapterScheduler, PetriNetService};
use petri_infrastructure::{MarkingProjection, MemoryEventStore, MemoryTopologyStore};

use aithericon_cli::client::EngineClient;

/// A lightweight in-memory engine HTTP server for integration tests.
pub struct TestServer {
    url: String,
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

impl TestServer {
    /// Start a single-net engine (basic mode, routes at `/api/*`).
    pub async fn start() -> Self {
        let event_repo = Arc::new(MemoryEventStore::new());
        let topology_repo = Arc::new(MemoryTopologyStore::new());
        let projection = Arc::new(MarkingProjection::new());
        let service = Arc::new(PetriNetService::new(event_repo, topology_repo, projection));

        let (event_tx, _) = broadcast::channel(256);
        let state = AppState {
            service,
            adapter_scheduler: Arc::new(AdapterScheduler::new()),
            run_mode: Arc::new(RwLock::new(RunMode::Stopped)),
            eval_notify: Arc::new(Notify::new()),
            event_tx: Arc::new(event_tx),
            // Sub-phase 2.5e-γ.mekhan scaffold field — single-net test
            // server uses default (empty) options. Per-test ablation
            // exercises happen via the scenario-load envelope, not via
            // this constructor.
            dispatch_options: Arc::new(RwLock::new(petri_domain::DispatchOptions::default())),
        };

        let router = axum::Router::new().nest("/api", create_router(state));
        Self::serve(router).await
    }

    /// Start a multi-net engine (registry mode, routes at `/api/nets/{net_id}/*`).
    ///
    /// Matches production: each net is isolated with its own stores.
    /// Deploy nets via `deploy_net("net-id", &scenario)`.
    pub async fn multi_net() -> Self {
        let factory: StoreFactory<MemoryEventStore, MemoryTopologyStore, MarkingProjection> =
            Arc::new(|_net_id: &str| {
                let (_tx, rx) = tokio::sync::watch::channel(0u64);
                (
                    Arc::new(MemoryEventStore::new()),
                    Arc::new(MemoryTopologyStore::new()),
                    Arc::new(MarkingProjection::new()),
                    rx,
                )
            });

        let registry = Arc::new(NetRegistry::new(factory));
        let router = create_router_with_registry(registry);
        Self::serve(router).await
    }

    async fn serve(router: axum::Router) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    rx.await.ok();
                })
                .await
                .ok();
        });

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        Self { url, _shutdown: tx }
    }

    /// Base URL for this test server.
    // Shared test helper: used by some integration files, not all.
    #[allow(dead_code)]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Get an EngineClient pointing at this test server.
    pub fn client(&self) -> EngineClient {
        EngineClient::new(&self.url)
    }

    // ── Single-net helpers ──────────────────────────────────────────────

    /// Deploy a scenario (single-net mode, POST /api/scenario).
    ///
    /// Sub-phase 2.5e-γ.mekhan-S2: the handler now expects a
    /// `LoadScenarioRequest` envelope (`{"scenario": <...>}`) rather than
    /// a bare `ScenarioDefinition`. We wrap on the way out so existing
    /// callers passing bare scenarios continue to work.
    pub async fn deploy(&self, scenario: &serde_json::Value) {
        let envelope = serde_json::json!({"scenario": scenario});
        self.post("/api/scenario", &envelope).await;
    }

    /// Evaluate (single-net mode, POST /api/command/evaluate).
    pub async fn evaluate(&self, max_steps: usize) {
        self.post(
            "/api/command/evaluate",
            &serde_json::json!({"max_steps": max_steps}),
        )
        .await;
    }

    // ── Multi-net helpers ───────────────────────────────────────────────

    /// Deploy a scenario to a named net (multi-net mode).
    ///
    /// Sub-phase 2.5e-γ.mekhan-S2: see `deploy` doc; same envelope wrap
    /// applies to the net-scoped scenario-load endpoint.
    pub async fn deploy_net(&self, net_id: &str, scenario: &serde_json::Value) {
        let envelope = serde_json::json!({"scenario": scenario});
        self.post(&format!("/api/nets/{net_id}/scenario"), &envelope)
            .await;
    }

    /// Evaluate a named net (multi-net mode).
    pub async fn evaluate_net(&self, net_id: &str, max_steps: usize) {
        self.post(
            &format!("/api/nets/{net_id}/command/evaluate"),
            &serde_json::json!({"max_steps": max_steps}),
        )
        .await;
    }

    /// Set run-mode for a named net (multi-net mode).
    // Shared test helper: used by some integration files, not all.
    #[allow(dead_code)]
    pub async fn set_run_mode(&self, net_id: &str, mode: &str) -> serde_json::Value {
        self.put(
            &format!("/api/nets/{net_id}/run-mode"),
            &serde_json::json!({"mode": mode}),
        )
        .await
    }

    /// PUT that returns (status_code, body) instead of panicking on non-2xx.
    pub async fn put_raw(&self, path: &str, body: &serde_json::Value) -> (u16, String) {
        let url = format!("{}{}", self.url, path);
        let body_str = body.to_string();
        tokio::task::spawn_blocking(move || {
            match ureq::put(&url)
                .set("Content-Type", "application/json")
                .send_string(&body_str)
            {
                Ok(resp) => {
                    let code = resp.status();
                    let body = resp.into_string().unwrap_or_default();
                    (code, body)
                }
                Err(ureq::Error::Status(code, resp)) => {
                    let body = resp.into_string().unwrap_or_default();
                    (code, body)
                }
                Err(e) => panic!("PUT {url} failed: {e}"),
            }
        })
        .await
        .unwrap()
    }

    // ── Generic HTTP helpers (async-safe) ───────────────────────────────

    /// GET a JSON endpoint.
    pub async fn get(&self, path: &str) -> serde_json::Value {
        let url = format!("{}{}", self.url, path);
        tokio::task::spawn_blocking(move || match ureq::get(&url).call() {
            Ok(resp) => resp
                .into_json::<serde_json::Value>()
                .expect("parse GET response"),
            Err(ureq::Error::Status(code, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                panic!("GET {url} failed with {code}: {body}");
            }
            Err(e) => panic!("GET {url} failed: {e}"),
        })
        .await
        .unwrap()
    }

    /// PUT with JSON body.
    // Shared test helper: used by some integration files, not all.
    #[allow(dead_code)]
    pub async fn put(&self, path: &str, body: &serde_json::Value) -> serde_json::Value {
        let url = format!("{}{}", self.url, path);
        let body_str = body.to_string();
        tokio::task::spawn_blocking(move || {
            match ureq::put(&url)
                .set("Content-Type", "application/json")
                .send_string(&body_str)
            {
                Ok(resp) => resp
                    .into_json::<serde_json::Value>()
                    .expect("parse PUT response"),
                Err(ureq::Error::Status(code, resp)) => {
                    let body = resp.into_string().unwrap_or_default();
                    panic!("PUT {url} failed with {code}: {body}");
                }
                Err(e) => panic!("PUT {url} failed: {e}"),
            }
        })
        .await
        .unwrap()
    }

    /// POST with JSON body.
    pub async fn post(&self, path: &str, body: &serde_json::Value) -> serde_json::Value {
        let url = format!("{}{}", self.url, path);
        let body_str = body.to_string();
        tokio::task::spawn_blocking(move || {
            match ureq::post(&url)
                .set("Content-Type", "application/json")
                .send_string(&body_str)
            {
                Ok(resp) => resp
                    .into_json::<serde_json::Value>()
                    .expect("parse POST response"),
                Err(ureq::Error::Status(code, resp)) => {
                    let body = resp.into_string().unwrap_or_default();
                    panic!("POST {url} failed with {code}: {body}");
                }
                Err(e) => panic!("POST {url} failed: {e}"),
            }
        })
        .await
        .unwrap()
    }
}
