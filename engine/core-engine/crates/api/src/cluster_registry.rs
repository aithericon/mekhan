//! The engine `ClusterRegistry` — per-cluster connection management for the
//! multi-cluster scheduling work (docs/16).
//!
//! ## What it replaces
//!
//! Before this module the engine spoke to exactly ONE external cluster, built
//! from `SLURM_*`/`NOMAD_*` env at boot: a single
//! [`FlavorDispatchAllocatorClient`](crate::slurm_allocator::FlavorDispatchAllocatorClient)
//! registered once per net (`net_registry.rs`), plus a single
//! `SlurmWatcher`/`NomadWatcher` started at boot (`main.rs`). The
//! `ClusterRegistry` instead builds a per-`(resource_id, version)`
//! [`ClusterClient`] LAZILY on the first lease/submit that references that
//! cluster, from the connection that rides the effect_config (mekhan resolves
//! the datacenter resource → inlines the non-secret connection + the wrapped
//! secret; the engine stays Vault-ignorant). One engine serves N clusters.
//!
//! ## Lifecycle (cluster Wake-Run-Hibernate, the net-hibernation analogue)
//!
//! - **Lazy build:** [`ClusterRegistry::get_or_build`] is the build-on-first-fire
//!   entrypoint. Read-lock fast path on `(resource_id, version)`; on miss, build
//!   the allocator (and start a per-cluster watcher under `run_with_reconnect`)
//!   OUTSIDE the write lock's await (mirrors `NetRegistry::get_or_create`'s
//!   factory-outside-lock discipline — never hold the lock across SSH connect).
//! - **Active counter:** every held lease / in-flight submit bumps
//!   [`ClusterClient`]'s active count; the watcher's signal deliveries bump
//!   `last_used` (a held-but-quiet lease is NOT idle).
//! - **Idle-teardown:** when the active count transitions to 0, an idle-grace
//!   timer is armed; on wake it double-checks `active == 0 && last_used > grace`
//!   (defends the acquire-during-grace race) and, if still idle, stops the
//!   watcher, drops the allocator (SSH session closes on drop), removes the
//!   client from the map, and deletes the temp PEM file. A fire arriving cancels
//!   teardown.
//! - **Reconnect isolation:** each cluster's watcher runs under its OWN
//!   `run_with_reconnect(shutdown_rx, "cluster-<resource_id>")` so one cluster
//!   flapping backs off on ITS task only — siblings untouched.
//!
//! ## Cache key `(resource_id, version)`
//!
//! A datacenter version bump (new ssh key / moved host) builds a FRESH client;
//! the old version's client idles out. Keying on `resource_id` alone would pin a
//! stale connection.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use parking_lot::RwLock as SyncRwLock;
use tokio::sync::broadcast;
use tokio::sync::RwLock;

use petri_application::resource_lease_handlers::{
    AllocatorClient, AllocatorError, HttpAllocatorClient,
};

/// Default idle grace before a quiet (active==0) cluster client is torn down.
const DEFAULT_IDLE_GRACE: Duration = Duration::from_secs(120);

/// Reserved cache key for the single env-driven dev-bootstrap cluster. A real
/// datacenter `resource_id` is a UUID, so it can never collide with this
/// literal (matching the watchers' `DEV_BOOTSTRAP_CLUSTER_KEY`).
pub const DEV_BOOTSTRAP_RESOURCE_ID: &str = "_env";

type ClientMap = HashMap<(String, i32), Arc<ClusterClient>>;

// ---------------------------------------------------------------------------
// Resolved connection parsed out of the effect_config
// ---------------------------------------------------------------------------

/// The full resolved cluster connection the lease/submit effect parses out of
/// its per-fire `effect_config` (mekhan inlined the non-secret fields + the
/// wrapped secret, `firing.rs` unwrapped the secret just-in-time). This is the
/// engine-side analogue of the datacenter resource's connection — it carries
/// every per-flavor field plus the two correlation keys (`resource_id`,
/// `resource_version`) that form the [`ClusterRegistry`] cache key.
#[derive(Clone, Debug, Default)]
pub struct ClusterConnection {
    /// Cache key part 1 — the datacenter `resource_id` (a UUID, or
    /// [`DEV_BOOTSTRAP_RESOURCE_ID`] for the env client). Absent in legacy
    /// http-only configs → defaults to the dev-bootstrap id.
    pub resource_id: String,
    /// Cache key part 2 — the datacenter resource version. Absent → 0.
    pub version: i32,
    /// `"http"` | `"slurm"` | `"nomad"`. Selects which leg `get_or_build`
    /// constructs.
    pub flavor: String,

    // ── generic HTTP leg (flavor == "http") ────────────────────────────────
    pub allocator_url: String,
    pub token: String,

    // ── slurm leg (flavor == "slurm") ──────────────────────────────────────
    pub ssh_host: Option<String>,
    pub ssh_port: Option<u16>,
    pub ssh_user: Option<String>,
    /// Inline PEM private key material (already unwrapped from Vault). The
    /// registry writes it to a 0600 temp file held on the [`ClusterClient`].
    pub ssh_key: Option<String>,
    pub ssh_known_hosts: Option<String>,
    pub template_dir: Option<String>,

    // ── nomad leg (flavor == "nomad") ──────────────────────────────────────
    pub nomad_addr: Option<String>,
    pub nomad_region: Option<String>,
    pub nomad_token: Option<String>,
}

impl ClusterConnection {
    /// The `(resource_id, version)` cache key.
    pub fn cache_key(&self) -> (String, i32) {
        (self.resource_id.clone(), self.version)
    }

    /// Parse a [`ClusterConnection`] out of the resolved per-fire `effect_config`
    /// (mekhan inlined the non-secret fields + the wrapped secret; `firing.rs`
    /// unwrapped the secret just-in-time). All fields are lenient: a legacy
    /// http-only config (no `resource_id`/`resource_version`) resolves to the
    /// dev-bootstrap cache key + the http leg, byte-identical to the old path.
    pub fn from_effect_config(config: &serde_json::Value) -> Self {
        let s = |k: &str| config.get(k).and_then(|v| v.as_str()).map(str::to_string);
        ClusterConnection {
            resource_id: s("resource_id").unwrap_or_else(|| DEV_BOOTSTRAP_RESOURCE_ID.to_string()),
            version: config
                .get("resource_version")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32,
            flavor: s("scheduler_flavor").unwrap_or_else(|| "http".to_string()),
            allocator_url: s("allocator_url").unwrap_or_default(),
            token: s("token").unwrap_or_default(),
            ssh_host: s("ssh_host"),
            ssh_port: config
                .get("ssh_port")
                .and_then(|v| v.as_u64())
                .map(|p| p as u16),
            ssh_user: s("ssh_user"),
            ssh_key: s("ssh_key"),
            ssh_known_hosts: s("ssh_known_hosts"),
            template_dir: s("template_dir"),
            nomad_addr: s("nomad_addr"),
            nomad_region: s("nomad_region"),
            nomad_token: s("nomad_token"),
        }
    }
}

// ---------------------------------------------------------------------------
// Health / observability (the GET /api/clusters payload source)
// ---------------------------------------------------------------------------

/// Connection-health state for a cluster client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnHealth {
    Connected,
    Reconnecting,
    Down,
    Unknown,
}

impl ConnHealth {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConnHealth::Connected => "connected",
            ConnHealth::Reconnecting => "reconnecting",
            ConnHealth::Down => "down",
            ConnHealth::Unknown => "unknown",
        }
    }
}

/// Watcher state for a cluster client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatcherState {
    Streaming,
    Reconnecting,
    Stopped,
    NoWatcher,
}

impl WatcherState {
    pub fn as_str(&self) -> &'static str {
        match self {
            WatcherState::Streaming => "streaming",
            WatcherState::Reconnecting => "reconnecting",
            WatcherState::Stopped => "stopped",
            WatcherState::NoWatcher => "no_watcher",
        }
    }
}

/// Mutable health/observability for a single cluster, surfaced via
/// `GET /api/clusters`. Updated by the watcher + allocator legs.
pub struct ClusterHealth {
    connection_health: SyncRwLock<ConnHealth>,
    watcher_state: SyncRwLock<WatcherState>,
    cursor: SyncRwLock<Option<String>>,
    last_signal_at: SyncRwLock<Option<DateTime<Utc>>>,
    last_error: SyncRwLock<Option<String>>,
}

impl ClusterHealth {
    fn new(watcher_state: WatcherState) -> Self {
        Self {
            connection_health: SyncRwLock::new(ConnHealth::Unknown),
            watcher_state: SyncRwLock::new(watcher_state),
            cursor: SyncRwLock::new(None),
            last_signal_at: SyncRwLock::new(None),
            last_error: SyncRwLock::new(None),
        }
    }

    pub fn set_connection_health(&self, h: ConnHealth) {
        *self.connection_health.write() = h;
    }
    pub fn set_watcher_state(&self, s: WatcherState) {
        *self.watcher_state.write() = s;
    }
    pub fn set_cursor(&self, c: Option<String>) {
        *self.cursor.write() = c;
    }
    pub fn set_last_error(&self, e: Option<String>) {
        *self.last_error.write() = e;
    }
    pub fn mark_signal(&self) {
        *self.last_signal_at.write() = Some(Utc::now());
    }

    pub fn connection_health(&self) -> ConnHealth {
        *self.connection_health.read()
    }
    pub fn watcher_state(&self) -> WatcherState {
        *self.watcher_state.read()
    }
    pub fn cursor(&self) -> Option<String> {
        self.cursor.read().clone()
    }
    pub fn last_signal_at(&self) -> Option<DateTime<Utc>> {
        *self.last_signal_at.read()
    }
    pub fn last_error(&self) -> Option<String> {
        self.last_error.read().clone()
    }
}

// ---------------------------------------------------------------------------
// ClusterClient
// ---------------------------------------------------------------------------

/// A spawned watcher's shutdown handle.
struct WatcherHandle {
    shutdown: broadcast::Sender<()>,
    handle: tokio::task::JoinHandle<()>,
}

/// One live connection to one external cluster: the allocator the lease effects
/// route to, plus the watcher streaming that cluster's job/alloc signals.
pub struct ClusterClient {
    pub resource_id: String,
    pub version: i32,
    pub flavor: String,
    /// The flavor-specific allocator (`SlurmAllocatorClient` / `NomadAllocatorClient`
    /// / `HttpAllocatorClient`). The lease effects route acquire/release here.
    pub allocator: Arc<dyn AllocatorClient>,
    /// Watcher shutdown sender + join handle (`None` for the http leg — no
    /// watcher). Sending on the broadcast stops `run_with_reconnect`.
    watcher: Option<WatcherHandle>,
    /// Health/observability surfaced via `GET /api/clusters`.
    pub health: Arc<ClusterHealth>,
    /// Held lease + in-flight submit reference count. Idle-teardown fires at 0.
    active: AtomicUsize,
    /// Last fire/signal timestamp — the second idle guard alongside `active`.
    last_used: SyncRwLock<Instant>,
    /// `true` once a `drain` request lands: `get_or_build` refuses new active
    /// increments so in-flight leases finish then the client idle-tears-down.
    draining: AtomicBool,
    /// Held temp PEM file (slurm) — kept alive as long as the client so the
    /// 0600 key the `SlurmConfig.ssh_key` path points at survives. Dropping the
    /// client deletes it.
    _ssh_key_file: Option<tempfile::NamedTempFile>,
}

impl std::fmt::Debug for ClusterClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterClient")
            .field("resource_id", &self.resource_id)
            .field("version", &self.version)
            .field("flavor", &self.flavor)
            .field("active", &self.active_count())
            .field("has_watcher", &self.watcher.is_some())
            .finish_non_exhaustive()
    }
}

impl ClusterClient {
    /// Active reference count (held leases + in-flight submits).
    pub fn active_count(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    /// Whether this client is draining (no new fires accepted).
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Bump `last_used` to now (called on fire + on watcher signal delivery so a
    /// held-but-quiet lease is never seen as idle).
    pub fn touch(&self) {
        *self.last_used.write() = Instant::now();
    }

    fn last_used(&self) -> Instant {
        *self.last_used.read()
    }

    /// Stop the watcher task (shutdown signal + abort the handle). Idempotent.
    async fn stop_watcher(&self) {
        if let Some(ref w) = self.watcher {
            let _ = w.shutdown.send(());
            w.handle.abort();
        }
        self.health.set_watcher_state(WatcherState::Stopped);
    }
}

// ---------------------------------------------------------------------------
// ClusterRegistry
// ---------------------------------------------------------------------------

/// Lazily-built, idle-torn-down per-cluster clients keyed by
/// `(resource_id, version)`.
pub struct ClusterRegistry {
    /// `Arc`-wrapped so the spawned idle-teardown task can hold a `'static`
    /// handle to the map without a self-referential `Arc<ClusterRegistry>`.
    clients: Arc<RwLock<ClientMap>>,
    /// JetStream context the per-cluster watchers' `SignalPublisher` +
    /// `CheckpointStore` are built from. `None` only in unit tests that exercise
    /// the http leg + cache/teardown machinery (which never spawn a watcher).
    jetstream: Option<async_nats::jetstream::Context>,
    /// Shared generic-HTTP allocator (stateless; reused for every http-flavor
    /// cluster — no per-cluster connection state for the http leg).
    http_allocator: Arc<dyn AllocatorClient>,
    /// Idle grace before a quiet client is torn down.
    idle_grace: Duration,
}

impl ClusterRegistry {
    /// Build a fresh registry. The dev-bootstrap client (if `SLURM_SSH_HOST` /
    /// `NOMAD_ADDR` is set) is installed separately so construction stays
    /// synchronous (the watcher start is async).
    pub fn new(jetstream: async_nats::jetstream::Context) -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            jetstream: Some(jetstream),
            http_allocator: Arc::new(HttpAllocatorClient::new()),
            idle_grace: DEFAULT_IDLE_GRACE,
        }
    }

    /// Test-only constructor with NO JetStream context. The http leg +
    /// cache/active-count/teardown machinery never touch jetstream; a
    /// slurm/nomad build under this constructor returns an explicit error.
    #[cfg(test)]
    fn new_for_test() -> Self {
        Self {
            clients: Arc::new(RwLock::new(HashMap::new())),
            jetstream: None,
            http_allocator: Arc::new(HttpAllocatorClient::new()),
            idle_grace: DEFAULT_IDLE_GRACE,
        }
    }

    /// Override the idle-teardown grace (tests use a short window).
    pub fn with_idle_grace(mut self, grace: Duration) -> Self {
        self.idle_grace = grace;
        self
    }

    /// Snapshot of every live cluster client (for `GET /api/clusters`).
    pub async fn list(&self) -> Vec<Arc<ClusterClient>> {
        self.clients.read().await.values().cloned().collect()
    }

    /// Number of live cluster clients.
    pub async fn len(&self) -> usize {
        self.clients.read().await.len()
    }

    /// Whether no cluster clients are live.
    pub async fn is_empty(&self) -> bool {
        self.clients.read().await.is_empty()
    }

    /// Look up a live client by `resource_id` (latest version present).
    pub async fn get_by_resource_id(&self, resource_id: &str) -> Option<Arc<ClusterClient>> {
        self.clients
            .read()
            .await
            .iter()
            .filter(|((rid, _), _)| rid == resource_id)
            .max_by_key(|((_, ver), _)| *ver)
            .map(|(_, c)| c.clone())
    }

    /// THE lazy build-on-first-fire entrypoint. Resolve (or build) the
    /// [`ClusterClient`] for this connection's `(resource_id, version)`, bump its
    /// active count + `last_used`, and return it. The caller routes
    /// acquire/submit through `client.allocator` then calls [`release`] when the
    /// lease ends.
    ///
    /// [`release`]: ClusterRegistry::release
    pub async fn get_or_build(
        &self,
        conn: &ClusterConnection,
    ) -> Result<Arc<ClusterClient>, AllocatorError> {
        let key = conn.cache_key();

        // Fast path: read-lock only.
        if let Some(client) = self.clients.read().await.get(&key).cloned() {
            return self.acquire_ref(client);
        }

        // Miss: build the client OUTSIDE the write lock (may block on SSH
        // connect / watcher start), mirroring `NetRegistry::get_or_create`.
        let built = self.build_client(conn).await?;

        let mut clients = self.clients.write().await;
        // Double-check: another fire may have built it while we connected.
        if let Some(client) = clients.get(&key).cloned() {
            // Drop our freshly-built one and stop its watcher; use the winner.
            drop(clients);
            built.stop_watcher().await;
            return self.acquire_ref(client);
        }
        let out = built.clone();
        clients.insert(key, built);
        drop(clients);
        self.acquire_ref(out)
    }

    /// Bump a resolved client's active count (rejecting if draining) + touch it.
    fn acquire_ref(
        &self,
        client: Arc<ClusterClient>,
    ) -> Result<Arc<ClusterClient>, AllocatorError> {
        if client.is_draining() {
            return Err(AllocatorError::BadResponse(format!(
                "cluster {} is draining — no new leases accepted",
                client.resource_id
            )));
        }
        client.active.fetch_add(1, Ordering::SeqCst);
        client.touch();
        Ok(client)
    }

    /// Decrement a cluster's active count after a lease releases (or a submit
    /// completes). When it reaches 0, arm the idle-teardown timer.
    pub async fn release(&self, resource_id: &str, version: i32) {
        let key = (resource_id.to_string(), version);
        let client = { self.clients.read().await.get(&key).cloned() };
        let Some(client) = client else {
            return;
        };
        // Atomic-floored decrement. A plain load-then-`fetch_sub` is a TOCTOU:
        // two concurrent releases on `active==1` (cancel racing the loop's
        // natural `t_release`) both read prev==1, both `fetch_sub`, and the
        // second underflows 0 → `usize::MAX` (AtomicUsize wraps), pinning the
        // counter high so idle-teardown can NEVER reap the client — the watcher
        // task + SSH ControlMaster socket + Nomad stream + temp PEM leak until
        // engine restart. `fetch_update` makes the over-release a no-op: the
        // closure returns `None` at 0, so the CAS yields `Err` and the counter
        // can't wrap. Only the transition that actually drove active → 0 arms
        // teardown.
        let prev = client
            .active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                if v == 0 {
                    None
                } else {
                    Some(v - 1)
                }
            });
        if prev == Ok(1) {
            self.arm_idle_teardown(key);
        }
    }

    /// Build the per-cluster client (allocator + optional watcher) for `conn`.
    /// Picks the leg by flavor — the registry IS the dispatcher now.
    async fn build_client(
        &self,
        conn: &ClusterConnection,
    ) -> Result<Arc<ClusterClient>, AllocatorError> {
        match conn.flavor.as_str() {
            "http" | "" => Ok(self.build_http_client(conn)),
            "slurm" => self.build_slurm_client(conn).await,
            "nomad" => self.build_nomad_client(conn).await,
            other => Err(AllocatorError::BadResponse(format!(
                "unknown scheduler_flavor {other:?} — expected http|slurm|nomad"
            ))),
        }
    }

    /// The http leg: stateless shared allocator, no watcher.
    fn build_http_client(&self, conn: &ClusterConnection) -> Arc<ClusterClient> {
        Arc::new(ClusterClient {
            resource_id: conn.resource_id.clone(),
            version: conn.version,
            flavor: "http".to_string(),
            allocator: self.http_allocator.clone(),
            watcher: None,
            health: Arc::new(ClusterHealth::new(WatcherState::NoWatcher)),
            active: AtomicUsize::new(0),
            last_used: SyncRwLock::new(Instant::now()),
            draining: AtomicBool::new(false),
            _ssh_key_file: None,
        })
    }

    /// Arm the idle-teardown sleep task for a quiet cluster. On wake it
    /// double-checks `active == 0 && last_used > grace` before reaping — a fire
    /// arriving during the grace bumps `active`/`last_used` and cancels teardown.
    fn arm_idle_teardown(&self, key: (String, i32)) {
        let grace = self.idle_grace;
        let clients = self.clients.clone();
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let mut map = clients.write().await;
            let Some(client) = map.get(&key).cloned() else {
                return; // already gone
            };
            // Double-check both guards.
            if client.active_count() != 0 {
                return; // re-acquired during grace
            }
            if client.last_used().elapsed() < grace {
                return; // touched during grace (held-but-quiet lease / signal)
            }
            // Still idle → reap. Remove from the map first so no new fire finds
            // it, then stop the watcher; dropping the last Arc closes the SSH
            // session + deletes the temp PEM file.
            map.remove(&key);
            drop(map);
            client.stop_watcher().await;
            tracing::info!(
                resource_id = %client.resource_id,
                version = client.version,
                "cluster idle-teardown: reaped quiet cluster client",
            );
        });
    }

    /// Force-reconnect a cluster: shut its watcher so `run_with_reconnect`
    /// re-enters its connect arm; the next fire rebuilds the allocator (new SSH
    /// session). Held leases keep their Arc alive until release. Returns `false`
    /// if no such cluster is live.
    pub async fn force_reconnect(&self, resource_id: &str) -> bool {
        let Some(client) = self.get_by_resource_id(resource_id).await else {
            return false;
        };
        client.health.set_watcher_state(WatcherState::Reconnecting);
        client.stop_watcher().await;
        let key = (client.resource_id.clone(), client.version);
        self.clients.write().await.remove(&key);
        true
    }

    /// Mark a cluster draining: refuse new active increments, let in-flight
    /// leases finish, then idle-teardown. Returns `false` if not live.
    pub async fn drain_cluster(&self, resource_id: &str) -> bool {
        let Some(client) = self.get_by_resource_id(resource_id).await else {
            return false;
        };
        client.draining.store(true, Ordering::SeqCst);
        // If already quiet, arm teardown immediately.
        if client.active_count() == 0 {
            self.arm_idle_teardown((client.resource_id.clone(), client.version));
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Flavor-specific builders (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "slurm")]
impl ClusterRegistry {
    async fn build_slurm_client(
        &self,
        conn: &ClusterConnection,
    ) -> Result<Arc<ClusterClient>, AllocatorError> {
        use petri_slurm::{SlurmConfig, SlurmConnectionParams};

        let ssh_host = conn.ssh_host.clone().ok_or_else(|| {
            AllocatorError::BadResponse("slurm cluster connection missing ssh_host".into())
        })?;
        let ssh_user = conn.ssh_user.clone().ok_or_else(|| {
            AllocatorError::BadResponse("slurm cluster connection missing ssh_user".into())
        })?;

        // Materialise the inline PEM to a 0600 temp file; SlurmConfig.ssh_key is
        // a PATH field. Hold the NamedTempFile on the client so the key lives as
        // long as the cluster (dropping the client deletes it).
        let ssh_key_file = match conn.ssh_key.as_deref().filter(|k| !k.is_empty()) {
            Some(pem) => Some(write_pem_tempfile(pem)?),
            None => None,
        };
        let ssh_key_path = ssh_key_file
            .as_ref()
            .map(|f| f.path().to_string_lossy().to_string());

        let params = SlurmConnectionParams {
            ssh_host,
            ssh_port: conn.ssh_port,
            ssh_user,
            ssh_key: ssh_key_path,
            ssh_known_hosts: conn.ssh_known_hosts.clone(),
            template_dir: conn.template_dir.clone(),
        };
        let config = SlurmConfig::from_connection(params);

        let allocator: Arc<dyn AllocatorClient> = Arc::new(
            crate::slurm_allocator::SlurmAllocatorClient::from_connection(config.clone()),
        );

        let health = Arc::new(ClusterHealth::new(WatcherState::Reconnecting));
        let watcher = self
            .spawn_slurm_watcher(config, &conn.resource_id, health.clone())
            .await;

        Ok(Arc::new(ClusterClient {
            resource_id: conn.resource_id.clone(),
            version: conn.version,
            flavor: "slurm".to_string(),
            allocator,
            watcher,
            health,
            active: AtomicUsize::new(0),
            last_used: SyncRwLock::new(Instant::now()),
            draining: AtomicBool::new(false),
            _ssh_key_file: ssh_key_file,
        }))
    }

    /// Spawn the per-cluster Slurm watcher under its OWN `run_with_reconnect`.
    async fn spawn_slurm_watcher(
        &self,
        config: petri_slurm::SlurmConfig,
        resource_id: &str,
        health: Arc<ClusterHealth>,
    ) -> Option<WatcherHandle> {
        let Some(jetstream) = self.jetstream.clone() else {
            health.set_watcher_state(WatcherState::NoWatcher);
            return None;
        };
        let watcher = match petri_slurm::SlurmWatcher::from_connection(
            config,
            jetstream,
            resource_id.to_string(),
        )
        .await
        {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::warn!(
                    resource_id = %resource_id,
                    error = %e,
                    "failed to build Slurm watcher for cluster — leases work, signals won't stream",
                );
                health.set_watcher_state(WatcherState::Stopped);
                return None;
            }
        };
        Some(spawn_cluster_watcher(resource_id, health, move |rx| {
            let w = watcher.clone();
            async move {
                w.run(rx).await;
            }
        }))
    }
}

#[cfg(not(feature = "slurm"))]
impl ClusterRegistry {
    async fn build_slurm_client(
        &self,
        _conn: &ClusterConnection,
    ) -> Result<Arc<ClusterClient>, AllocatorError> {
        Err(AllocatorError::BadResponse(
            "scheduler_flavor=slurm but the engine was built without the `slurm` feature".into(),
        ))
    }
}

#[cfg(feature = "nomad")]
impl ClusterRegistry {
    async fn build_nomad_client(
        &self,
        conn: &ClusterConnection,
    ) -> Result<Arc<ClusterClient>, AllocatorError> {
        use petri_nomad::{NomadConfig, NomadConnectionParams};

        let addr = conn.nomad_addr.clone().ok_or_else(|| {
            AllocatorError::BadResponse("nomad cluster connection missing nomad_addr".into())
        })?;
        let params = NomadConnectionParams {
            addr,
            token: conn.nomad_token.clone(),
            region: conn.nomad_region.clone(),
            ca_cert: None,
        };
        let config = NomadConfig::from_connection(params);

        let allocator: Arc<dyn AllocatorClient> = Arc::new(
            crate::nomad_allocator::NomadAllocatorClient::from_connection(config.clone())?,
        );

        let health = Arc::new(ClusterHealth::new(WatcherState::Reconnecting));
        let watcher = self
            .spawn_nomad_watcher(config, &conn.resource_id, health.clone())
            .await;

        Ok(Arc::new(ClusterClient {
            resource_id: conn.resource_id.clone(),
            version: conn.version,
            flavor: "nomad".to_string(),
            allocator,
            watcher,
            health,
            active: AtomicUsize::new(0),
            last_used: SyncRwLock::new(Instant::now()),
            draining: AtomicBool::new(false),
            _ssh_key_file: None,
        }))
    }

    async fn spawn_nomad_watcher(
        &self,
        config: petri_nomad::NomadConfig,
        resource_id: &str,
        health: Arc<ClusterHealth>,
    ) -> Option<WatcherHandle> {
        let Some(jetstream) = self.jetstream.clone() else {
            health.set_watcher_state(WatcherState::NoWatcher);
            return None;
        };
        let watcher = match petri_nomad::NomadWatcher::from_connection(
            config,
            jetstream,
            resource_id.to_string(),
        )
        .await
        {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::warn!(
                    resource_id = %resource_id,
                    error = %e,
                    "failed to build Nomad watcher for cluster — leases work, signals won't stream",
                );
                health.set_watcher_state(WatcherState::Stopped);
                return None;
            }
        };
        Some(spawn_cluster_watcher(resource_id, health, move |rx| {
            let w = watcher.clone();
            async move {
                w.run(rx).await;
            }
        }))
    }
}

#[cfg(not(feature = "nomad"))]
impl ClusterRegistry {
    async fn build_nomad_client(
        &self,
        _conn: &ClusterConnection,
    ) -> Result<Arc<ClusterClient>, AllocatorError> {
        Err(AllocatorError::BadResponse(
            "scheduler_flavor=nomad but the engine was built without the `nomad` feature".into(),
        ))
    }
}

/// Spawn a cluster watcher under its OWN cluster-scoped `run_with_reconnect`, so
/// one cluster's flap backs off on ITS task only (reconnect isolation). `run`
/// is invoked with a fresh shutdown receiver each reconnect; the returned
/// [`WatcherHandle`] owns the same sender so stopping the watcher (teardown /
/// force-reconnect) signals the loop.
#[cfg(any(feature = "slurm", feature = "nomad"))]
fn spawn_cluster_watcher<F, Fut>(
    resource_id: &str,
    health: Arc<ClusterHealth>,
    run_once: F,
) -> WatcherHandle
where
    F: Fn(broadcast::Receiver<()>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    let (shutdown_tx, _rx0) = broadcast::channel::<()>(1);
    let loop_rx = shutdown_tx.subscribe();
    let inner_tx = shutdown_tx.clone();
    let label = format!("cluster-{resource_id}");
    let handle = tokio::spawn(async move {
        health.set_watcher_state(WatcherState::Streaming);
        petri_scheduler_bridge::backoff::run_with_reconnect(loop_rx, &label, || {
            // Each reconnect gets a fresh receiver so a stream-end (Ok) re-runs;
            // a shutdown send stops both this receiver and the outer loop_rx.
            let fut = run_once(inner_tx.subscribe());
            async move {
                fut.await;
                Ok::<(), std::convert::Infallible>(())
            }
        })
        .await;
        health.set_watcher_state(WatcherState::Stopped);
    });
    WatcherHandle {
        shutdown: shutdown_tx,
        handle,
    }
}

/// Write an inline PEM private key to a 0600 temp file, returning the held
/// handle (the registry keeps it alive on the `ClusterClient`).
#[cfg(feature = "slurm")]
fn write_pem_tempfile(pem: &str) -> Result<tempfile::NamedTempFile, AllocatorError> {
    use std::io::Write;
    let mut f = tempfile::Builder::new()
        .prefix("cluster-ssh-key-")
        .tempfile()
        .map_err(|e| AllocatorError::Transport(format!("temp key file: {e}")))?;
    f.write_all(pem.as_bytes())
        .map_err(|e| AllocatorError::Transport(format!("write temp key: {e}")))?;
    f.flush()
        .map_err(|e| AllocatorError::Transport(format!("flush temp key: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(f.path(), perms)
            .map_err(|e| AllocatorError::Transport(format!("chmod 0600 temp key: {e}")))?;
    }
    Ok(f)
}

// ---------------------------------------------------------------------------
// The AllocatorClient adapter — the seam the lease handlers hold
// ---------------------------------------------------------------------------

/// An [`AllocatorClient`] that delegates to a [`ClusterRegistry`]. This is the
/// client the per-net `ResourceLease{Acquire,Release}Handler`s hold (registered
/// in `net_registry.rs`) — it folds the old `FlavorDispatchAllocatorClient` into
/// the registry's `get_or_build` flavor match. On acquire it parses the
/// per-fire `effect_config` into a [`ClusterConnection`], lazily builds/resolves
/// the right [`ClusterClient`] (bumping that cluster's active count), and routes
/// the acquire to its allocator. On release it routes the allocator release then
/// decrements the active count (arming idle-teardown at 0).
///
/// petri-application's handlers cannot hold the concrete `ClusterRegistry`
/// (layering: application does not depend on api), so this adapter lives in
/// petri-api and is injected as `Arc<dyn AllocatorClient>`.
pub struct ClusterRegistryAllocatorClient {
    registry: Arc<ClusterRegistry>,
}

impl ClusterRegistryAllocatorClient {
    pub fn new(registry: Arc<ClusterRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl AllocatorClient for ClusterRegistryAllocatorClient {
    // The bare trait methods are a flavorless (http) fallback for any direct
    // caller. The registry-aware path is `acquire_with_connection` /
    // `release_with_connection` below (the handlers always call those).
    async fn acquire(
        &self,
        allocator_url: &str,
        token: &str,
        grant_id: &str,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, AllocatorError> {
        let conn = ClusterConnection {
            resource_id: DEV_BOOTSTRAP_RESOURCE_ID.to_string(),
            flavor: "http".to_string(),
            allocator_url: allocator_url.to_string(),
            token: token.to_string(),
            ..Default::default()
        };
        let client = self.registry.get_or_build(&conn).await?;
        client
            .allocator
            .acquire(allocator_url, token, grant_id, request)
            .await
    }

    async fn release(
        &self,
        allocator_url: &str,
        token: &str,
        alloc_id: &str,
    ) -> Result<(), AllocatorError> {
        // No cache key on the bare path → reuse the http leg directly via a
        // fresh dev-bootstrap resolve; release is idempotent.
        let conn = ClusterConnection {
            resource_id: DEV_BOOTSTRAP_RESOURCE_ID.to_string(),
            flavor: "http".to_string(),
            allocator_url: allocator_url.to_string(),
            token: token.to_string(),
            ..Default::default()
        };
        let client = self.registry.get_or_build(&conn).await?;
        let res = client.allocator.release(allocator_url, token, alloc_id).await;
        self.registry
            .release(DEV_BOOTSTRAP_RESOURCE_ID, 0)
            .await;
        res
    }

    async fn acquire_with_connection(
        &self,
        config: &serde_json::Value,
        grant_id: &str,
        request: &serde_json::Value,
    ) -> Result<serde_json::Value, AllocatorError> {
        let conn = ClusterConnection::from_effect_config(config);
        let client = self.registry.get_or_build(&conn).await?;
        // Route the acquire through the resolved cluster's allocator. NOTE: the
        // active count was bumped by `get_or_build` and is held until release.
        client
            .allocator
            .acquire_with_flavor(
                &conn.flavor,
                &conn.allocator_url,
                &conn.token,
                grant_id,
                request,
            )
            .await
    }

    async fn release_with_connection(
        &self,
        config: &serde_json::Value,
        alloc_id: &str,
    ) -> Result<(), AllocatorError> {
        let conn = ClusterConnection::from_effect_config(config);
        // Resolve the (already-built, held) cluster. `get_or_build` bumps the
        // active count by 1 for THIS release call; we drop both that and the
        // acquire's hold below (two `release`s).
        let client = self.registry.get_or_build(&conn).await?;
        let res = client
            .allocator
            .release_with_flavor(&conn.flavor, &conn.allocator_url, &conn.token, alloc_id)
            .await;
        // Drop the increment this call took…
        self.registry.release(&conn.resource_id, conn.version).await;
        // …and the increment the matching acquire took (the held lease ends).
        self.registry.release(&conn.resource_id, conn.version).await;
        res
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An http connection (no flavor → the stateless shared allocator, no
    /// watcher) so tests need NO NATS/SSH — the `new_for_test` registry carries
    /// no jetstream context and the http leg never touches one.
    fn http_conn(resource_id: &str, version: i32) -> ClusterConnection {
        ClusterConnection {
            resource_id: resource_id.to_string(),
            version,
            flavor: "http".to_string(),
            allocator_url: "http://localhost:9".to_string(),
            token: String::new(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn http_build_caches_by_resource_id_and_version() {
        let reg = ClusterRegistry::new_for_test();
        let conn = http_conn("dc-1", 3);

        let c1 = reg.get_or_build(&conn).await.unwrap();
        assert_eq!(c1.active_count(), 1);
        assert_eq!(c1.flavor, "http");
        assert!(matches!(c1.health.watcher_state(), WatcherState::NoWatcher));

        // Second fire on the SAME (resource_id, version) reuses the client.
        let c2 = reg.get_or_build(&conn).await.unwrap();
        assert!(Arc::ptr_eq(&c1, &c2));
        assert_eq!(c1.active_count(), 2);
        assert_eq!(reg.len().await, 1);

        // A version bump builds a FRESH client (distinct cache key) — the stale
        // old version must not be reused.
        let c3 = reg.get_or_build(&http_conn("dc-1", 4)).await.unwrap();
        assert!(!Arc::ptr_eq(&c1, &c3));
        assert_eq!(reg.len().await, 2);

        reg.release("dc-1", 3).await;
        assert_eq!(c1.active_count(), 1);
        reg.release("dc-1", 3).await;
        assert_eq!(c1.active_count(), 0);
        // Unbalanced release must not underflow.
        reg.release("dc-1", 3).await;
        assert_eq!(c1.active_count(), 0);
    }

    #[tokio::test]
    async fn idle_teardown_reaps_quiet_client_but_acquire_during_grace_cancels() {
        let reg = ClusterRegistry::new_for_test().with_idle_grace(Duration::from_millis(80));
        let conn = http_conn("dc-2", 1);

        let c = reg.get_or_build(&conn).await.unwrap();
        reg.release("dc-2", 1).await; // active → 0, arms teardown
        assert_eq!(c.active_count(), 0);

        // Re-acquire BEFORE the grace elapses — must cancel teardown (active>0).
        let _c2 = reg.get_or_build(&conn).await.unwrap();
        tokio::time::sleep(Duration::from_millis(140)).await;
        assert_eq!(reg.len().await, 1, "re-acquire during grace must keep it");

        // Now release and let the grace fully elapse → reaped.
        reg.release("dc-2", 1).await;
        tokio::time::sleep(Duration::from_millis(160)).await;
        assert_eq!(reg.len().await, 0, "quiet client must be torn down");
    }

    #[tokio::test]
    async fn draining_refuses_new_leases() {
        let reg = ClusterRegistry::new_for_test();
        let conn = http_conn("dc-3", 1);
        let _c = reg.get_or_build(&conn).await.unwrap(); // active = 1 (in-flight)

        assert!(reg.drain_cluster("dc-3").await);
        let err = reg.get_or_build(&conn).await.unwrap_err();
        match err {
            AllocatorError::BadResponse(msg) => assert!(msg.contains("draining")),
            other => panic!("expected BadResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_flavor_is_a_hard_error_not_a_silent_http_route() {
        let reg = ClusterRegistry::new_for_test();
        let conn = ClusterConnection {
            resource_id: "dc-4".to_string(),
            version: 1,
            flavor: "k8s".to_string(),
            ..Default::default()
        };
        let err = reg.get_or_build(&conn).await.unwrap_err();
        match err {
            AllocatorError::BadResponse(msg) => {
                assert!(msg.contains("k8s"));
                assert!(msg.contains("http|slurm|nomad"));
            }
            other => panic!("expected BadResponse, got {other:?}"),
        }
        assert_eq!(reg.len().await, 0, "a bad-flavor fire must not cache a client");
    }

    #[test]
    fn dev_bootstrap_key_can_never_equal_a_uuid_resource_id() {
        // The dup-seq guard: the env client's cache key is a literal, never a
        // UUID, so a resource-driven cluster never collides with it.
        assert!(uuid::Uuid::parse_str(DEV_BOOTSTRAP_RESOURCE_ID).is_err());
    }
}
