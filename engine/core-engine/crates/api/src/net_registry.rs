//! Multi-net registry: manages multiple isolated Petri net instances in a single process.
//!
//! Note on interior mutability asymmetry: most `set_*` configuration methods
//! take `&mut self` and run during single-threaded setup, but
//! `register_pre_dispatch_hook` takes `&self` so consumers (e.g. cloud-layer
//! capability-routing) can register hooks from initialisation paths that
//! already hold an `Arc<NetRegistry>`. The pre-dispatch hook table is guarded
//! by its own `RwLock` + an `AtomicBool` frozen flag (see
//! `pre-dispatch-hook.md` § 6 — registration MUST happen before the first
//! `get_or_create` call; after that point the registry is frozen and
//! `RegistrationError::RegistryFrozen` is returned).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::{broadcast, Notify};
use tokio_util::sync::CancellationToken;

use crate::slurm_allocator::FlavorDispatchAllocatorClient;
use petri_application::pre_dispatch::{
    HttpPreDispatchHook, PreDispatchChain, PreDispatchChainEntry, PreDispatchHook,
    PreDispatchHookConfig, PreDispatchRuntime, PreDispatchTransport, RegistrationError,
};
use petri_application::resource_lease_handlers::AllocatorClient;
use petri_application::{
    subworkflow_handlers::SubWorkflowCancelHandler, AdapterScheduler, EventRepository,
    HttpAllocatorClient, MockSchedulerClient, PetriNetService, ProcessCompleteHandler,
    ProcessFailHandler, ProcessLogMessageHandler, ProcessLogMetricHandler, ProcessStartHandler,
    ProcessStatusDetailHandler, ResourceLeaseAcquireHandler, ResourceLeaseReleaseHandler,
    SchedulerCancelHandler, SchedulerSubmitHandler, StateProjection, TimerCancelHandler,
    TimerScheduleHandler, TopologyRepository,
};
#[cfg(feature = "catalogue")]
use petri_application::{
    CatalogueLookupHandler, CatalogueRegisterHandler, CatalogueSubscribeHandler,
    CatalogueUnsubscribeHandler,
};
#[cfg(feature = "executor")]
use petri_application::{ExecutorCancelHandler, ExecutorStreamFeedHandler, ExecutorSubmitHandler};
use petri_domain::human::HumanTaskClient;
#[cfg(feature = "executor")]
use petri_domain::ExecutorClient;
use petri_domain::{
    effects, subworkflow::SubWorkflowCancellor, timer::TimerClient, PlaceId, SchedulerClient,
};

use crate::dto::RunMode;
use crate::router::{AppState, SseSignal};

/// Result of a metadata lookup for a net ID.
///
/// Used by [`MetadataLookup`] to communicate the externally-persisted
/// status of a net so the registry can decide whether to rehydrate it
/// or refuse the request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetadataStatus {
    /// Net is known and active — safe to rehydrate.
    Known,
    /// Net reached a terminal state (completed or cancelled). Reject requests.
    Tombstoned,
    /// Net has no metadata entry — it was never deployed.
    Unknown,
}

/// External lookup for net metadata, used to rehydrate hibernated nets on
/// control-plane requests (e.g. setting run-mode, querying state) after a
/// cold engine boot.
///
/// In production this is backed by the `KV_NET_METADATA` JetStream KV bucket;
/// tests can leave it unset and rely on the in-process `known_nets` set.
#[async_trait::async_trait]
pub trait MetadataLookup: Send + Sync {
    async fn lookup(&self, net_id: &str) -> MetadataStatus;
}

/// Factory function type for creating human task clients per net.
pub type HumanClientFactory = Arc<dyn Fn(&str) -> Arc<dyn HumanTaskClient> + Send + Sync>;

/// Configuration for human task integration.
#[derive(Clone)]
pub struct HumanIntegrationConfig {
    /// Factory to create a human task client for a specific net.
    /// The factory receives the net_id and returns a client configured for that net.
    pub client_factory: HumanClientFactory,
}

/// Configuration for the external scheduler backend.
///
/// When set on the `NetRegistry`, every new net instance will have
/// `scheduler_submit` and `scheduler_cancel` effect handlers registered
/// automatically.
#[derive(Clone, Debug)]
pub struct SchedulerConfig {
    /// Which scheduler backend to use.
    pub backend: SchedulerBackend,
    /// Default job template ID for submit handlers.
    pub job_template_id: String,
}

/// Available scheduler backends.
#[derive(Clone, Debug)]
pub enum SchedulerBackend {
    /// In-process mock scheduler for testing.
    Mock,
    /// HashiCorp Nomad scheduler.
    #[cfg(feature = "nomad")]
    Nomad {
        /// Nomad connection config.
        config: petri_nomad::NomadConfig,
        /// Fallback signal place for statuses not in `signal_routes`.
        /// Also stamped as `petri_place` for backward compatibility.
        fallback_place: String,
        /// Per-status signal routing: status name → place name.
        signal_routes: std::collections::HashMap<String, String>,
    },
    /// Slurm scheduler via SSH + CLI.
    #[cfg(feature = "slurm")]
    Slurm {
        /// Slurm SSH connection config (boxed to avoid large enum variant).
        config: Box<petri_slurm::SlurmConfig>,
        /// Fallback signal place for statuses not in `signal_routes`.
        fallback_place: String,
        /// Per-status signal routing: status name → place name.
        signal_routes: std::collections::HashMap<String, String>,
    },
}

/// Configuration for the executor integration.
///
/// When set on the `NetRegistry`, every new net instance will have
/// `executor_submit` and `executor_cancel` effect handlers registered.
#[cfg(feature = "executor")]
#[derive(Clone)]
pub struct ExecutorIntegrationConfig {
    /// NATS client for the executor (ephemeral publishes like cancel).
    pub nats_client: async_nats::Client,
    /// JetStream context for the executor (durable publishes like submit).
    pub jetstream: async_nats::jetstream::Context,
    /// apalis-nats job namespace.
    pub namespace: String,
    /// Fallback signal place for statuses not in `signal_routes`.
    pub fallback_place: String,
    /// Per-status signal routing: status name -> place name.
    pub signal_routes: std::collections::HashMap<String, String>,
    /// Per-category event routing: category -> place name.
    pub event_routes: std::collections::HashMap<String, String>,
    /// Secret store for resolving `{{secret:KEY}}` refs before wrapping.
    #[cfg(feature = "executor-vault-secrets")]
    pub secret_store: Option<Arc<dyn aithericon_secrets::SecretStore>>,
    /// Wrapper for creating single-use Vault wrapping tokens.
    #[cfg(feature = "executor-vault-secrets")]
    pub secret_wrapper: Option<Arc<dyn aithericon_secrets::SecretWrapper>>,
}

#[cfg(feature = "executor")]
impl std::fmt::Debug for ExecutorIntegrationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutorIntegrationConfig")
            .field("namespace", &self.namespace)
            .field("fallback_place", &self.fallback_place)
            .finish_non_exhaustive()
    }
}

/// Configuration for the HTTP-dispatch executor integration (sub-phase 2.3b).
///
/// When set on the `NetRegistry`, every new net instance will have an
/// HTTP-based `executor_submit` handler ([`HttpInferenceHandler`]) registered.
/// The handler reads cap-routing's pre-dispatch enrichment (`base_url` +
/// `lease_token`) from `EffectInput.config` and dispatches inference
/// synchronously via HTTP to `{base_url}/v1/inference` (the endpoint added in
/// `executor-llm/src/inference_handler.rs`).
///
/// Mutually exclusive with [`ExecutorIntegrationConfig`] (NATS dispatch);
/// `get_or_create` panics if both are set on the registry.
///
/// No `executor_cancel` is registered in HTTP-sync mode — there is no
/// in-flight job to cancel from outside (the handler's `submit` is
/// synchronous). Cancellation in HTTP-sync mode is a separate workstream.
///
/// [`HttpInferenceHandler`]: petri_application::http_executor_client::HttpInferenceHandler
#[cfg(feature = "executor")]
#[derive(Clone, Debug)]
pub struct HttpExecutorConfig {
    /// Input port name. Defaults to `EXECUTOR_SUBMIT.default_input_port`
    /// (`"job"`) so scenarios authored against the NATS handler stay
    /// portable.
    pub input_port: String,
    /// Output port name. Defaults to `EXECUTOR_SUBMIT.default_output_port`
    /// (`"submitted"`).
    pub output_port: String,
}

#[cfg(feature = "executor")]
impl Default for HttpExecutorConfig {
    fn default() -> Self {
        Self {
            input_port: effects::EXECUTOR_SUBMIT.default_input_port.to_string(),
            output_port: effects::EXECUTOR_SUBMIT.default_output_port.to_string(),
        }
    }
}

/// Configuration for the data catalogue integration.
///
/// When set on the `NetRegistry`, every new net instance will have
/// catalogue effect handlers registered automatically.
#[cfg(feature = "catalogue")]
#[derive(Clone)]
pub struct CatalogueIntegrationConfig {
    /// Core NATS client for request-reply queries (lookup, subscribe, unsubscribe).
    pub nats_client: async_nats::Client,
}

/// Async callback invoked after a scenario is loaded (e.g., to reset a NATS consumer).
pub type OnScenarioLoaded =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send + Sync>;

/// A single isolated Petri net instance with its own stores, eval loop, and state.
pub struct NetInstance<E, T, S>
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    pub net_id: String,
    pub service: Arc<PetriNetService<E, T, S>>,
    pub adapter_scheduler: Arc<AdapterScheduler>,
    pub run_mode: Arc<RwLock<RunMode>>,
    pub eval_notify: Arc<Notify>,
    /// Broadcast sender for SSE event streaming.
    pub event_tx: Arc<broadcast::Sender<SseSignal>>,
    /// Callbacks invoked after a scenario is loaded.
    /// Used to reset durable NATS consumers so stale signals from a previous
    /// scenario instance are not delivered.
    pub on_scenario_loaded: RwLock<Vec<OnScenarioLoaded>>,
    /// Cancellation token for graceful shutdown of per-net tasks (eval loop, listeners).
    pub cancel_token: CancellationToken,
    /// Sub-phase 2.5e-γ.mekhan per-run dispatch options (skip_mask +
    /// stage_overrides). Owned here per-NetInstance so concurrent loads on
    /// distinct net_ids never collide. `as_app_state` clones the Arc into
    /// the per-request AppState facade.
    pub dispatch_options: Arc<RwLock<petri_domain::DispatchOptions>>,
}

impl<E, T, S> NetInstance<E, T, S>
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    /// Run all `on_scenario_loaded` callbacks (e.g., reset NATS consumers).
    pub async fn notify_scenario_loaded(&self) {
        let callbacks: Vec<_> = self.on_scenario_loaded.read().clone();
        for cb in callbacks {
            cb().await;
        }
    }

    /// Build an `AppState` from this instance's fields, for reuse with existing handlers.
    pub fn as_app_state(&self) -> AppState<E, T, S> {
        AppState {
            service: self.service.clone(),
            adapter_scheduler: self.adapter_scheduler.clone(),
            run_mode: self.run_mode.clone(),
            eval_notify: self.eval_notify.clone(),
            event_tx: self.event_tx.clone(),
            dispatch_options: self.dispatch_options.clone(),
        }
    }
}

/// Factory function type for creating fresh stores when a new net is instantiated.
///
/// Receives the `net_id` so the factory can configure per-net stores (e.g., set the
/// net ID on a NATS publisher for correct bridge routing).
///
/// Returns `(event_store, topology_store, projection, applied_rx)`.
/// The `applied_rx` watch channel ticks every time the event consumer applies
/// an event to the in-memory cache, enabling consumer-driven eval notification.
pub type StoreFactory<E, T, S> =
    Arc<dyn Fn(&str) -> (Arc<E>, Arc<T>, Arc<S>, tokio::sync::watch::Receiver<u64>) + Send + Sync>;

/// Optional callback invoked after a new net instance is created (e.g., to start a bridge listener).
pub type OnNetCreated<E, T, S> = Arc<dyn Fn(&Arc<NetInstance<E, T, S>>) + Send + Sync>;

/// Registry that manages multiple Petri net instances in a single process.
pub struct NetRegistry<E, T, S>
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    #[allow(clippy::type_complexity)]
    nets: RwLock<HashMap<String, Arc<NetInstance<E, T, S>>>>,
    /// All net IDs that were ever created or registered. Survives hibernation
    /// so we can distinguish "hibernated" (rehydratable) from "never existed".
    known_nets: RwLock<std::collections::HashSet<String>>,
    store_factory: StoreFactory<E, T, S>,
    on_create: Option<OnNetCreated<E, T, S>>,
    scheduler_config: Option<SchedulerConfig>,
    timer_client: Option<Arc<dyn TimerClient>>,
    /// Cancellor for child nets. The Timeout node's body-cancellation path
    /// fires `subworkflow_cancel` effects to terminate spawned child nets.
    /// Wrapped in `RwLock` because main.rs installs it after the registry is
    /// already `Arc`-wrapped (the cancellor needs `Arc<NetRegistry>` itself
    /// to call `terminate`, which creates a one-way cycle resolved at use
    /// time). See `set_subworkflow_cancellor` and [`RegistryCancellor`].
    subworkflow_cancellor: RwLock<Option<Arc<dyn SubWorkflowCancellor>>>,
    /// The multi-cluster `ClusterRegistry` (docs/16). When set, the per-net
    /// `ResourceLease{Acquire,Release}` handlers are registered with a client
    /// that delegates to it (lazy per-cluster build + idle-teardown) instead of
    /// the boot-singleton `FlavorDispatchAllocatorClient`. Wrapped in `RwLock<
    /// Option<>>` + set via `&self` so main.rs can install it after the registry
    /// is `Arc`-wrapped (mirroring `subworkflow_cancellor`).
    #[cfg(any(feature = "slurm", feature = "nomad"))]
    cluster_registry: RwLock<Option<Arc<crate::cluster_registry::ClusterRegistry>>>,
    #[cfg(feature = "executor")]
    executor_config: Option<ExecutorIntegrationConfig>,
    #[cfg(feature = "executor")]
    http_executor_config: Option<HttpExecutorConfig>,
    human_config: Option<HumanIntegrationConfig>,
    #[cfg(feature = "catalogue")]
    catalogue_config: Option<CatalogueIntegrationConfig>,
    /// Optional external lookup so handlers can rehydrate hibernated nets
    /// after a cold engine boot (when `known_nets` is empty).
    metadata_lookup: Option<Arc<dyn MetadataLookup>>,
    /// Registered builtin pre-dispatch hooks, keyed by their `name`.
    /// Resolved against the TOML-config chain at net-instantiation time.
    pre_dispatch_builtin_hooks: RwLock<HashMap<String, Arc<dyn PreDispatchHook>>>,
    /// TOML-loaded `[[pre_dispatch_hooks]]` config entries (declaration order).
    pre_dispatch_chain_configs: RwLock<Vec<PreDispatchHookConfig>>,
    /// True once the first `get_or_create` runs — registration is rejected
    /// after this point with `RegistrationError::RegistryFrozen`.
    pre_dispatch_frozen: AtomicBool,
}

impl<E, T, S> NetRegistry<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    /// Create a new registry with a factory for creating fresh stores.
    pub fn new(store_factory: StoreFactory<E, T, S>) -> Self {
        Self {
            nets: RwLock::new(HashMap::new()),
            known_nets: RwLock::new(std::collections::HashSet::new()),
            store_factory,
            on_create: None,
            scheduler_config: None,
            timer_client: None,
            subworkflow_cancellor: RwLock::new(None),
            #[cfg(any(feature = "slurm", feature = "nomad"))]
            cluster_registry: RwLock::new(None),
            #[cfg(feature = "executor")]
            executor_config: None,
            #[cfg(feature = "executor")]
            http_executor_config: None,
            human_config: None,
            #[cfg(feature = "catalogue")]
            catalogue_config: None,
            metadata_lookup: None,
            pre_dispatch_builtin_hooks: RwLock::new(HashMap::new()),
            pre_dispatch_chain_configs: RwLock::new(Vec::new()),
            pre_dispatch_frozen: AtomicBool::new(false),
        }
    }

    /// Install the TOML-loaded `[[pre_dispatch_hooks]]` chain config. Must
    /// run before the first `get_or_create` — after that point the registry
    /// is frozen.
    pub fn set_pre_dispatch_chain_configs(
        &self,
        configs: Vec<PreDispatchHookConfig>,
    ) -> Result<(), RegistrationError> {
        if self.pre_dispatch_frozen.load(Ordering::SeqCst) {
            return Err(RegistrationError::RegistryFrozen(
                "<chain-config>".to_string(),
            ));
        }
        *self.pre_dispatch_chain_configs.write() = configs;
        Ok(())
    }

    /// Register a builtin pre-dispatch hook under the given name (see
    /// `pre-dispatch-hook.md` § 6).
    ///
    /// Takes `&self` deliberately to support late-binding from caller-side
    /// initialisation paths that hold the registry via `Arc<NetRegistry>` —
    /// this is the only registry method that uses interior mutability for
    /// configuration writes. Registration MUST happen before any
    /// `get_or_create` call; after that point the registry is frozen and
    /// this returns `RegistrationError::RegistryFrozen`.
    pub fn register_pre_dispatch_hook(
        &self,
        name: impl Into<String>,
        hook: Arc<dyn PreDispatchHook>,
    ) -> Result<(), RegistrationError> {
        let name = name.into();
        if self.pre_dispatch_frozen.load(Ordering::SeqCst) {
            return Err(RegistrationError::RegistryFrozen(name));
        }
        let mut hooks = self.pre_dispatch_builtin_hooks.write();
        if hooks.contains_key(&name) {
            return Err(RegistrationError::DuplicateName(name));
        }
        hooks.insert(name, hook);
        Ok(())
    }

    /// Assemble the immutable chain for a single net by walking the TOML
    /// config in declaration order, resolving each entry against (a) the
    /// registered builtin map and (b) the engine's HTTP-transport factory.
    ///
    /// Spec § 6 fail-fast: a `transport = "builtin"` entry whose `name` is
    /// not registered triggers a synthetic chain with that builtin missing
    /// (logged warning) — startup-correctness is the caller's responsibility
    /// (the engine init path SHOULD verify the chain assembles cleanly).
    fn build_pre_dispatch_chain(&self) -> Arc<PreDispatchChain> {
        let configs = self.pre_dispatch_chain_configs.read().clone();
        let hooks = self.pre_dispatch_builtin_hooks.read();
        let mut entries: Vec<PreDispatchChainEntry> = Vec::with_capacity(configs.len());
        for cfg in &configs {
            let hook: Arc<dyn PreDispatchHook> = match cfg.transport {
                PreDispatchTransport::Builtin => {
                    if let Some(h) = hooks.get(&cfg.name) {
                        h.clone()
                    } else {
                        tracing::warn!(
                            name = %cfg.name,
                            "Pre-dispatch builtin hook configured but not registered — skipping"
                        );
                        continue;
                    }
                }
                PreDispatchTransport::Http => {
                    let url = match cfg.url.as_deref() {
                        Some(u) => u.to_string(),
                        None => {
                            tracing::warn!(
                                name = %cfg.name,
                                "Pre-dispatch HTTP hook missing `url` field — skipping"
                            );
                            continue;
                        }
                    };
                    let timeout = std::time::Duration::from_millis(cfg.timeout_ms);
                    Arc::new(HttpPreDispatchHook::new(
                        cfg.name.clone(),
                        url,
                        timeout,
                        cfg.http_max_retries,
                    ))
                }
            };
            entries.push(PreDispatchChainEntry {
                hook,
                fail_open: cfg.fail_open,
                timeout: std::time::Duration::from_millis(cfg.timeout_ms),
                match_effect_handlers: cfg.match_effect_handlers.clone(),
            });
        }
        Arc::new(PreDispatchChain { entries })
    }

    /// Read-only access to whether the pre-dispatch registry has been
    /// frozen (i.e. at least one net has been instantiated).
    pub fn pre_dispatch_is_frozen(&self) -> bool {
        self.pre_dispatch_frozen.load(Ordering::SeqCst)
    }

    /// Configure an external metadata lookup so handlers can rehydrate
    /// hibernated nets after a cold engine boot.
    pub fn set_metadata_lookup(&mut self, lookup: Arc<dyn MetadataLookup>) {
        self.metadata_lookup = Some(lookup);
    }

    /// Returns the configured metadata lookup, if any.
    pub fn metadata_lookup(&self) -> Option<&Arc<dyn MetadataLookup>> {
        self.metadata_lookup.as_ref()
    }

    /// Set the human task integration config.
    pub fn set_human_config(&mut self, config: HumanIntegrationConfig) {
        self.human_config = Some(config);
    }

    /// Set the timer client for durable delays.
    pub fn set_timer_client(&mut self, client: Arc<dyn TimerClient>) {
        self.timer_client = Some(client);
    }

    /// Install the cancellor used by the `subworkflow_cancel` effect handler.
    /// Typically wired in main.rs as a thin adapter over
    /// `NetRegistry::terminate` (see [`RegistryCancellor`]) so the engine can
    /// cancel its own child nets. Takes `&self` so it can be called after
    /// the registry is `Arc`-wrapped; must be set before any net that wants
    /// to use the handler is created.
    pub fn set_subworkflow_cancellor(&self, cancellor: Arc<dyn SubWorkflowCancellor>) {
        *self.subworkflow_cancellor.write() = Some(cancellor);
    }

    /// Install the multi-cluster `ClusterRegistry` (docs/16). After this, every
    /// new net's `resource_lease_acquire`/`resource_lease_release` handlers route
    /// through a registry-backed `ClusterRegistryAllocatorClient` (lazy
    /// per-cluster build + idle-teardown) instead of the boot-singleton
    /// `FlavorDispatchAllocatorClient`. Takes `&self` so main.rs can call it
    /// after the registry is `Arc`-wrapped; must be set before the first
    /// `get_or_create`. The registry is also held by main.rs for the
    /// `GET /api/clusters` management surface.
    #[cfg(any(feature = "slurm", feature = "nomad"))]
    pub fn set_cluster_registry(&self, registry: Arc<crate::cluster_registry::ClusterRegistry>) {
        *self.cluster_registry.write() = Some(registry);
    }

    /// Set a callback to run after each new net instance is created.
    ///
    /// Use this to wire up per-net infrastructure like NATS bridge listeners.
    pub fn set_on_create(&mut self, callback: OnNetCreated<E, T, S>) {
        self.on_create = Some(callback);
    }

    /// Configure an external scheduler backend.
    ///
    /// When set, every new net instance will have `scheduler_submit` and
    /// `scheduler_cancel` effect handlers registered automatically.
    pub fn set_scheduler_config(&mut self, config: SchedulerConfig) {
        self.scheduler_config = Some(config);
    }

    /// Configure the executor integration.
    ///
    /// When set, every new net instance will have `executor_submit` and
    /// `executor_cancel` effect handlers registered automatically.
    #[cfg(feature = "executor")]
    pub fn set_executor_config(&mut self, config: ExecutorIntegrationConfig) {
        self.executor_config = Some(config);
    }

    /// Configure the HTTP-dispatch executor integration (sub-phase 2.3b).
    ///
    /// When set, every new net instance will have the HTTP-based
    /// `executor_submit` handler ([`petri_application::http_executor_client::HttpInferenceHandler`])
    /// registered. Mutually exclusive with [`set_executor_config`] —
    /// `get_or_create` panics if both have been set on the registry.
    #[cfg(feature = "executor")]
    pub fn set_http_executor_config(&mut self, config: HttpExecutorConfig) {
        self.http_executor_config = Some(config);
    }

    /// Configure the data catalogue integration.
    ///
    /// When set, every new net instance will have `catalogue_register`
    /// effect handler registered automatically.
    #[cfg(feature = "catalogue")]
    pub fn set_catalogue_config(&mut self, config: CatalogueIntegrationConfig) {
        self.catalogue_config = Some(config);
    }

    /// Look up an existing net instance by ID (in-memory only).
    pub fn get(&self, net_id: &str) -> Option<Arc<NetInstance<E, T, S>>> {
        self.nets.read().get(net_id).cloned()
    }

    /// Check if a net ID was ever registered (survives hibernation).
    pub fn is_known(&self, net_id: &str) -> bool {
        self.known_nets.read().contains(net_id)
    }

    /// Get an existing net or create a new one with fresh isolated stores.
    ///
    /// When creating, spawns an evaluation loop for the new net.
    ///
    /// The store factory is called **outside** the registry lock so that
    /// long-running hydration (e.g. NATS event replay) cannot deadlock the
    /// registry. In the rare case two threads try to create the same net
    /// concurrently, the second thread's stores are discarded.
    pub fn get_or_create(&self, net_id: &str) -> Arc<NetInstance<E, T, S>> {
        // Fast path: check if it exists (read lock only)
        if let Some(instance) = self.nets.read().get(net_id).cloned() {
            return instance;
        }

        // Freeze the pre-dispatch registry BEFORE store factory + chain
        // assembly — spec § 11 trip-wire 7: the flip must be ordered against
        // hot-net creation so concurrent registration cannot slip through.
        self.pre_dispatch_frozen.store(true, Ordering::SeqCst);

        // Call factory OUTSIDE any lock — this may block on hydration
        let (event_store, topology_store, projection, applied_rx) = (self.store_factory)(net_id);

        // Acquire write lock for setup + insertion
        let mut nets = self.nets.write();
        // Double-check: another thread may have created it while we were hydrating
        if let Some(instance) = nets.get(net_id).cloned() {
            return instance; // Discard stores — another thread won the race
        }

        let service = Arc::new(PetriNetService::new(
            event_store,
            topology_store,
            projection,
        ));

        // Register all effect handlers (scheduler/executor/human/process/
        // timer/subworkflow/catalogue) configured on this registry.
        self.register_effect_handlers(&service, net_id);

        // Bind pre-dispatch hook runtime (chain + defer budgets) to this
        // service. The chain is assembled from the registered builtin map +
        // TOML config snapshot taken at freeze-time. Per-(net_id,
        // transition_id) defer budgets live on the runtime — global counter
        // is explicitly disallowed (spec § 11 trip-wire 4).
        let chain = self.build_pre_dispatch_chain();
        let chain_len = chain.len();
        let rt = Arc::new(PreDispatchRuntime::new(net_id, chain));
        service.set_pre_dispatch_runtime(rt);
        if chain_len > 0 {
            tracing::info!(
                net_id = %net_id,
                chain_len,
                "Bound pre-dispatch hook chain"
            );
        }

        let eval_notify = Arc::new(Notify::new());

        // Woken nets (re-hydrated from NATS) already have topology and marking;
        // they should resume in Running mode so eval fires on token injection.
        // Fresh nets start Stopped until a scenario is loaded via the API.
        let initial_mode = if service.get_topology().is_some() {
            tracing::info!(net_id = %net_id, "Waking from hibernation — resuming in Running mode");
            RunMode::Running
        } else {
            RunMode::Stopped
        };
        let run_mode = Arc::new(RwLock::new(initial_mode));
        let adapter_scheduler = Arc::new(AdapterScheduler::new());
        let (event_tx, _) = broadcast::channel::<SseSignal>(256);
        let event_tx = Arc::new(event_tx);
        let cancel_token = CancellationToken::new();

        let instance = Arc::new(NetInstance {
            net_id: net_id.to_string(),
            service: service.clone(),
            adapter_scheduler: adapter_scheduler.clone(),
            run_mode: run_mode.clone(),
            eval_notify: eval_notify.clone(),
            event_tx: event_tx.clone(),
            on_scenario_loaded: RwLock::new(Vec::new()),
            cancel_token: cancel_token.clone(),
            dispatch_options: Arc::new(RwLock::new(petri_domain::DispatchOptions::default())),
        });

        // Spawn evaluation loop for this net
        spawn_net_evaluation_loop(
            net_id.to_string(),
            service,
            adapter_scheduler,
            eval_notify.clone(),
            run_mode,
            event_tx,
            cancel_token.clone(),
        );

        // Consumer-driven eval notification: whenever the event consumer applies
        // an event to the in-memory cache, wake the eval loop. This eliminates
        // the race where a listener's notify_eval fires before the cache is
        // updated, causing the eval loop to find no new work and go back to sleep.
        {
            let kick = eval_notify;
            let mut rx = applied_rx;
            let cancel = cancel_token;
            let net_id_bridge = net_id.to_string();
            tokio::spawn(async move {
                tracing::info!(net_id = %net_id_bridge, "Consumer→eval bridge task started");
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => {
                            tracing::debug!(net_id = %net_id_bridge, "Consumer→eval bridge task cancelled");
                            return;
                        }
                        r = rx.changed() => {
                            if r.is_err() {
                                tracing::debug!(net_id = %net_id_bridge, "Consumer→eval bridge: applied_rx closed");
                                return;
                            }
                            let seq = *rx.borrow();
                            tracing::debug!(net_id = %net_id_bridge, applied_seq = seq, "Consumer→eval bridge: kicking eval");
                            kick.notify_one();
                        }
                    }
                }
            });
        }

        nets.insert(net_id.to_string(), instance.clone());
        drop(nets);

        // Track as known so it can be rehydrated after hibernation
        self.known_nets.write().insert(net_id.to_string());

        let on_create = self.on_create.clone();

        tracing::info!(net_id = %net_id, "Created new net instance");

        // Invoke the on-create callback (e.g., start bridge listener)
        if let Some(callback) = on_create {
            callback(&instance);
        }

        instance
    }

    /// Register every effect handler configured on this registry onto a
    /// freshly created `service`.
    ///
    /// Extracted verbatim from `get_or_create` — covers the
    /// scheduler/executor/human/process-lifecycle/timer/subworkflow/catalogue
    /// handler sets. Feature-gated registrations (`nomad`, `slurm`,
    /// `executor`, `executor-vault-secrets`, `catalogue`) are preserved
    /// exactly. Process lifecycle handlers are always registered.
    /// Build the legacy env-driven `FlavorDispatchAllocatorClient` (the
    /// fallback when no `ClusterRegistry` is installed): http + the optional
    /// `SLURM_*`/`NOMAD_*` env legs. Preserves the single-cluster dev recipes
    /// (`just dev slurm-up`/`scheduler-up`) that configure env, not a resource.
    fn build_env_flavor_dispatch() -> Arc<dyn AllocatorClient> {
        let http_allocator: Arc<dyn AllocatorClient> = Arc::new(HttpAllocatorClient::new());
        #[cfg(feature = "slurm")]
        let slurm_allocator: Option<Arc<dyn AllocatorClient>> =
            crate::slurm_allocator::SlurmAllocatorClient::from_env()
                .map(|c| Arc::new(c) as Arc<dyn AllocatorClient>);
        #[cfg(not(feature = "slurm"))]
        let slurm_allocator: Option<Arc<dyn AllocatorClient>> = None;
        #[cfg(feature = "nomad")]
        let nomad_allocator: Option<Arc<dyn AllocatorClient>> =
            crate::nomad_allocator::NomadAllocatorClient::from_env()
                .map(|c| Arc::new(c) as Arc<dyn AllocatorClient>);
        #[cfg(not(feature = "nomad"))]
        let nomad_allocator: Option<Arc<dyn AllocatorClient>> = None;
        Arc::new(FlavorDispatchAllocatorClient::new(
            http_allocator,
            slurm_allocator,
            nomad_allocator,
        ))
    }

    fn register_effect_handlers(
        &self,
        service: &std::sync::Arc<PetriNetService<E, T, S>>,
        net_id: &str,
    ) {
        // Register scheduler effect handlers if configured
        if let Some(ref cfg) = self.scheduler_config {
            let client: Arc<dyn SchedulerClient> = match &cfg.backend {
                SchedulerBackend::Mock => Arc::new(MockSchedulerClient::new(net_id)),
                #[cfg(feature = "nomad")]
                SchedulerBackend::Nomad {
                    config,
                    fallback_place,
                    signal_routes,
                } => match petri_nomad::NomadClient::new(
                    config.clone(),
                    net_id,
                    fallback_place.clone(),
                    signal_routes.clone(),
                ) {
                    Ok(client) => Arc::new(client),
                    Err(e) => {
                        tracing::error!(
                            error = %e,
                            net_id = %net_id,
                            "Failed to create NomadClient, falling back to mock"
                        );
                        Arc::new(MockSchedulerClient::new(net_id))
                    }
                },
                #[cfg(feature = "slurm")]
                SchedulerBackend::Slurm {
                    config,
                    fallback_place,
                    signal_routes,
                } => Arc::new(petri_slurm::SlurmClient::new(
                    config.as_ref().clone(),
                    net_id,
                    fallback_place.clone(),
                    signal_routes.clone(),
                )),
            };

            service
                .register_effect_handler(
                    effects::SCHEDULER_SUBMIT.handler_id,
                    Arc::new(SchedulerSubmitHandler::new(
                        client.clone(),
                        &cfg.job_template_id,
                        effects::SCHEDULER_SUBMIT.default_input_port,
                        effects::SCHEDULER_SUBMIT.default_output_port,
                    )),
                )
                .expect("register scheduler_submit effect handler");

            service
                .register_effect_handler(
                    effects::SCHEDULER_CANCEL.handler_id,
                    Arc::new(SchedulerCancelHandler::new(
                        client,
                        effects::SCHEDULER_CANCEL.default_input_port,
                        effects::SCHEDULER_CANCEL.default_output_port,
                    )),
                )
                .expect("register scheduler_cancel effect handler");

            tracing::info!(
                net_id = %net_id,
                backend = ?cfg.backend,
                template = %cfg.job_template_id,
                "Registered scheduler effect handlers",
            );
        }

        // Register executor effect handlers if configured
        #[cfg(feature = "executor")]
        if let Some(ref ecfg) = self.executor_config {
            let mut executor_nats_client = petri_executor::ExecutorNatsClient::new(
                ecfg.nats_client.clone(),
                ecfg.jetstream.clone(),
                net_id,
                &ecfg.fallback_place,
                ecfg.signal_routes.clone(),
                ecfg.event_routes.clone(),
                &ecfg.namespace,
            );

            // Wire up secret wrapping if configured
            #[cfg(feature = "executor-vault-secrets")]
            if let (Some(store), Some(wrapper)) = (&ecfg.secret_store, &ecfg.secret_wrapper) {
                executor_nats_client.set_secret_wrapping(store.clone(), wrapper.clone());
                // ALSO wire the Vault store into this net's evaluation service so
                // firing-time `resolve_secrets` (firing.rs) substitutes
                // `{{secret:<vault_path>#field}}` placeholders in an effect's
                // `effect_config` before the handler runs. This is what a
                // datacenter lease needs: the cluster CONNECTION (e.g. a Slurm
                // datacenter's inline `ssh_key` PEM) rides the
                // `resource_lease_acquire` effect_config as a secret template and
                // must be resolved to plaintext so the ClusterRegistry can write
                // the real key to its 0600 temp file. Without this the template
                // passes through LITERAL and the allocator's SSH fails with
                // "failed to connect" (a garbage key file). Nomad datacenters
                // dodge this — `nomad_addr` is non-secret public config — which is
                // why only the Slurm leg surfaced it.
                service.set_secret_store(store.clone());
                tracing::info!(net_id = %net_id, "Executor secret wrapping + firing-time secret resolution enabled");
            }

            let executor_client: Arc<dyn ExecutorClient> = Arc::new(executor_nats_client);

            service
                .register_effect_handler(
                    effects::EXECUTOR_SUBMIT.handler_id,
                    Arc::new(ExecutorSubmitHandler::new(
                        executor_client.clone(),
                        effects::EXECUTOR_SUBMIT.default_input_port,
                        effects::EXECUTOR_SUBMIT.default_output_port,
                    )),
                )
                .expect("register executor_submit effect handler");

            service
                .register_effect_handler(
                    effects::EXECUTOR_CANCEL.handler_id,
                    Arc::new(ExecutorCancelHandler::new(
                        executor_client.clone(),
                        effects::EXECUTOR_CANCEL.default_input_port,
                        effects::EXECUTOR_CANCEL.default_output_port,
                    )),
                )
                .expect("register executor_cancel effect handler");

            service
                .register_effect_handler(
                    effects::EXECUTOR_STREAM_FEED.handler_id,
                    Arc::new(ExecutorStreamFeedHandler::new(executor_client)),
                )
                .expect("register executor_stream_feed effect handler");

            tracing::info!(
                net_id = %net_id,
                namespace = %ecfg.namespace,
                "Registered executor effect handlers",
            );
        }

        // Register HTTP-dispatch executor handler if configured (sub-phase 2.3b).
        // Mutually exclusive with NATS dispatch above; panics at registration
        // if both configs are set.
        #[cfg(feature = "executor")]
        if let Some(ref hcfg) = self.http_executor_config {
            assert!(
                self.executor_config.is_none(),
                "NetRegistry: executor_config (NATS) and http_executor_config (HTTP) \
                 are mutually exclusive — set at most one"
            );

            service
                .register_effect_handler(
                    effects::EXECUTOR_SUBMIT.handler_id,
                    Arc::new(
                        petri_application::http_executor_client::HttpInferenceHandler::new(
                            hcfg.input_port.clone(),
                            hcfg.output_port.clone(),
                        ),
                    ),
                )
                .expect("register HTTP executor_submit effect handler");

            // Compiler-generated nets (graph→AIR) emit an `executor_cancel`
            // transition for every executor step, and deploy validation
            // requires the referenced handler to be registered. Under HTTP-sync
            // dispatch there is no async job to cancel, so register a no-op ack.
            service
                .register_effect_handler(
                    effects::EXECUTOR_CANCEL.handler_id,
                    Arc::new(
                        petri_application::http_executor_client::HttpExecutorCancelNoop::new(
                            effects::EXECUTOR_CANCEL.default_output_port,
                        ),
                    ),
                )
                .expect("register HTTP executor_cancel no-op effect handler");

            tracing::info!(
                net_id = %net_id,
                input_port = %hcfg.input_port,
                output_port = %hcfg.output_port,
                "Registered HTTP executor_submit + no-op executor_cancel handlers (cloud-layer dispatch)"
            );
        }

        // Register human task effect handlers if configured
        if let Some(ref hcfg) = self.human_config {
            // Create a per-net human client using the factory
            let human_client = (hcfg.client_factory)(net_id);

            service
                .register_effect_handler(
                    effects::HUMAN_TASK.handler_id,
                    Arc::new(petri_application::human_handlers::HumanTaskHandler::new(
                        human_client.clone(),
                        effects::HUMAN_TASK.default_input_port,
                        effects::HUMAN_TASK.default_output_port,
                    )),
                )
                .expect("register human_task effect handler");

            service
                .register_effect_handler(
                    effects::HUMAN_CANCEL.handler_id,
                    Arc::new(
                        petri_application::human_handlers::HumanTaskCancelHandler::new(
                            human_client,
                            effects::HUMAN_CANCEL.default_input_port,
                            effects::HUMAN_CANCEL.default_output_port,
                        ),
                    ),
                )
                .expect("register human_cancel effect handler");

            tracing::info!(net_id = %net_id, "Registered human task effect handlers (submit + cancel)");
        }

        // Register process lifecycle effect handlers (always — no tracker needed)
        service
            .register_effect_handler("process_start", Arc::new(ProcessStartHandler::new(net_id)))
            .expect("register process_start effect handler");

        service
            .register_effect_handler("process_complete", Arc::new(ProcessCompleteHandler::new()))
            .expect("register process_complete effect handler");

        service
            .register_effect_handler("process_fail", Arc::new(ProcessFailHandler::new()))
            .expect("register process_fail effect handler");

        service
            .register_effect_handler(
                effects::PROCESS_LOG_METRIC.handler_id,
                Arc::new(ProcessLogMetricHandler::new(
                    effects::PROCESS_LOG_METRIC.default_input_port,
                    effects::PROCESS_LOG_METRIC.default_output_port,
                )),
            )
            .expect("register process_log_metric effect handler");

        service
            .register_effect_handler(
                effects::PROCESS_LOG_MESSAGE.handler_id,
                Arc::new(ProcessLogMessageHandler::new(
                    effects::PROCESS_LOG_MESSAGE.default_input_port,
                    effects::PROCESS_LOG_MESSAGE.default_output_port,
                )),
            )
            .expect("register process_log_message effect handler");

        service
            .register_effect_handler(
                effects::PROCESS_PHASE.handler_id,
                Arc::new(ProcessStatusDetailHandler::new(
                    effects::PROCESS_PHASE.default_input_port,
                    effects::PROCESS_PHASE.default_output_port,
                    "process_phase",
                )),
            )
            .expect("register process_phase effect handler");

        service
            .register_effect_handler(
                effects::PROCESS_PROGRESS.handler_id,
                Arc::new(ProcessStatusDetailHandler::new(
                    effects::PROCESS_PROGRESS.default_input_port,
                    effects::PROCESS_PROGRESS.default_output_port,
                    "process_progress",
                )),
            )
            .expect("register process_progress effect handler");

        tracing::info!(net_id = %net_id, "Registered process lifecycle effect handlers (start + complete + fail + metric + log + phase + progress)");

        // Register resource-lease effect handlers (always — R4 `scheduler`
        // backend's `lease` operation). The allocator connection
        // (url + token) arrives per-FIRE via the transition's `effect_config`
        // (resolved from the datacenter resource secret just-in-time), NOT at
        // net-create — so one stateless allocator serves every datacenter, no
        // per-net connection state. Mirror the process-lifecycle always-on
        // block.
        //
        // The registered client is a `FlavorDispatchAllocatorClient` that routes
        // each fire on the `scheduler_flavor` the handler reads off the resolved
        // `effect_config` (default `"http"` → the generic HTTP allocator;
        // `"slurm"` → the SSH/salloc-backed `SlurmAllocatorClient`). The Slurm
        // leg is built from the `SLURM_*` env only when the `slurm` feature is on
        // AND `SLURM_SSH_HOST` is set; otherwise it is absent and a
        // `scheduler_flavor=slurm` fire fails loudly.
        // Multi-cluster (docs/16): when a `ClusterRegistry` is installed, route
        // lease acquire/release through a registry-backed client that lazily
        // builds a per-`(resource_id, version)` `ClusterClient` from the
        // connection riding the per-fire effect_config (and idle-tears-down when
        // a cluster has no held leases). This REPLACES the boot-singleton
        // `FlavorDispatchAllocatorClient` (folded into the registry's
        // `get_or_build` flavor match). When no registry is installed (e.g. a
        // plain http-allocator dev stack), fall back to the legacy dispatcher
        // built from the `SLURM_*`/`NOMAD_*` env.
        #[cfg(any(feature = "slurm", feature = "nomad"))]
        let cluster_registry = self.cluster_registry.read().clone();
        #[cfg(any(feature = "slurm", feature = "nomad"))]
        let allocator_client: Arc<dyn AllocatorClient> = match cluster_registry {
            Some(reg) => {
                Arc::new(crate::cluster_registry::ClusterRegistryAllocatorClient::new(reg))
            }
            None => Self::build_env_flavor_dispatch(),
        };
        #[cfg(not(any(feature = "slurm", feature = "nomad")))]
        let allocator_client: Arc<dyn AllocatorClient> = Self::build_env_flavor_dispatch();
        service
            .register_effect_handler(
                effects::RESOURCE_LEASE_ACQUIRE.handler_id,
                Arc::new(ResourceLeaseAcquireHandler::new(
                    allocator_client.clone(),
                    effects::RESOURCE_LEASE_ACQUIRE.default_input_port,
                    effects::RESOURCE_LEASE_ACQUIRE.default_output_port,
                )),
            )
            .expect("register resource_lease_acquire effect handler");
        service
            .register_effect_handler(
                effects::RESOURCE_LEASE_RELEASE.handler_id,
                Arc::new(ResourceLeaseReleaseHandler::new(
                    allocator_client,
                    effects::RESOURCE_LEASE_RELEASE.default_input_port,
                    effects::RESOURCE_LEASE_RELEASE.default_output_port,
                )),
            )
            .expect("register resource_lease_release effect handler");
        tracing::info!(net_id = %net_id, "Registered resource_lease effect handlers (acquire + release)");

        // Register timer effect handlers if configured
        if let Some(ref timer_client) = self.timer_client {
            service
                .register_effect_handler(
                    effects::TIMER_SCHEDULE.handler_id,
                    Arc::new(TimerScheduleHandler::new(
                        timer_client.clone(),
                        net_id,
                        effects::TIMER_SCHEDULE.default_input_port,
                        effects::TIMER_SCHEDULE.default_output_port,
                    )),
                )
                .expect("register timer_schedule effect handler");

            service
                .register_effect_handler(
                    effects::TIMER_CANCEL.handler_id,
                    Arc::new(TimerCancelHandler::new(
                        timer_client.clone(),
                        net_id,
                        effects::TIMER_CANCEL.default_input_port,
                        effects::TIMER_CANCEL.default_output_port,
                    )),
                )
                .expect("register timer_cancel effect handler");

            tracing::info!(net_id = %net_id, "Registered timer effect handlers");
        }

        // Register subworkflow cancel handler if configured (used by Timeout
        // node's body-cancellation post-pass to terminate child nets).
        if let Some(cancellor) = self.subworkflow_cancellor.read().clone() {
            service
                .register_effect_handler(
                    effects::SUBWORKFLOW_CANCEL.handler_id,
                    Arc::new(SubWorkflowCancelHandler::new(
                        cancellor,
                        effects::SUBWORKFLOW_CANCEL.default_input_port,
                        effects::SUBWORKFLOW_CANCEL.default_output_port,
                    )),
                )
                .expect("register subworkflow_cancel effect handler");

            tracing::info!(net_id = %net_id, "Registered subworkflow_cancel effect handler");
        }

        // Register catalogue effect handler if configured
        #[cfg(feature = "catalogue")]
        if let Some(ref ccfg) = self.catalogue_config {
            let client: Arc<dyn petri_domain::catalogue::CatalogueClient> =
                Arc::new(NatsCatalogueClient::new(ccfg.nats_client.clone()));

            service
                .register_effect_handler(
                    effects::CATALOGUE_REGISTER.handler_id,
                    Arc::new(CatalogueRegisterHandler::new(
                        effects::CATALOGUE_REGISTER.default_input_port,
                        effects::CATALOGUE_REGISTER.default_output_port,
                    )),
                )
                .expect("register catalogue_register effect handler");

            service
                .register_effect_handler(
                    effects::CATALOGUE_LOOKUP.handler_id,
                    Arc::new(CatalogueLookupHandler::new(
                        client.clone(),
                        effects::CATALOGUE_LOOKUP.default_input_port,
                        effects::CATALOGUE_LOOKUP.default_output_port,
                    )),
                )
                .expect("register catalogue_lookup effect handler");

            service
                .register_effect_handler(
                    effects::CATALOGUE_SUBSCRIBE.handler_id,
                    Arc::new(CatalogueSubscribeHandler::new(
                        client.clone(),
                        net_id,
                        effects::CATALOGUE_SUBSCRIBE.default_input_port,
                        effects::CATALOGUE_SUBSCRIBE.default_output_port,
                    )),
                )
                .expect("register catalogue_subscribe effect handler");

            service
                .register_effect_handler(
                    effects::CATALOGUE_UNSUBSCRIBE.handler_id,
                    Arc::new(CatalogueUnsubscribeHandler::new(
                        client.clone(),
                        effects::CATALOGUE_UNSUBSCRIBE.default_input_port,
                        effects::CATALOGUE_UNSUBSCRIBE.default_output_port,
                    )),
                )
                .expect("register catalogue_unsubscribe effect handler");

            tracing::info!(net_id = %net_id, "Registered catalogue effect handlers");
        }
    }

    /// Insert a pre-built net instance.
    pub fn insert(&self, net_id: &str, instance: Arc<NetInstance<E, T, S>>) {
        self.nets.write().insert(net_id.to_string(), instance);
    }

    /// List all registered net IDs.
    pub fn list(&self) -> Vec<String> {
        self.nets.read().keys().cloned().collect()
    }

    /// Remove a net instance by ID. Returns the removed instance if it existed.
    pub fn remove(&self, net_id: &str) -> Option<Arc<NetInstance<E, T, S>>> {
        let removed = self.nets.write().remove(net_id);
        if removed.is_some() {
            tracing::info!(net_id = %net_id, "Removed net instance");
        }
        removed
    }

    /// Hibernate a net: cancel its tasks and remove from memory.
    ///
    /// In-memory state is discarded; NATS JetStream retains all events
    /// for later rehydration via `get_or_create`.
    pub fn hibernate(&self, net_id: &str) -> Result<(), String> {
        let instance = self.nets.write().remove(net_id);
        match instance {
            Some(inst) => {
                inst.cancel_token.cancel();
                tracing::info!(net_id = %net_id, "Net hibernated (tasks cancelled, memory freed)");
                Ok(())
            }
            None => Err(format!("Net '{}' not found", net_id)),
        }
    }

    /// Pre-terminate hook (docs/16 §8) — release any cluster lease HELD on
    /// behalf of the instance `net_id` being cancelled, so a `scancel` /
    /// `nomad job stop` frees the held salloc + its persistent drain executor
    /// instead of leaking them.
    ///
    /// ## The leak this fixes
    ///
    /// `terminate` emits `NetCancelled` + hibernate, tearing down the eval loop
    /// BEFORE the leased loop reaches its `t_exit` (the natural-release path).
    /// So `t_release` never fires and the held salloc / dispatched drain job +
    /// its persistent executor + the cluster's watcher + the SSH ControlMaster
    /// socket all leak. This hook performs the forced release the torn-down loop
    /// would otherwise have done.
    ///
    /// ## How it finds the held lease
    ///
    /// The held lease lives on the lease-adapter pool-net (`pool-<resource_id>`),
    /// not on the instance net: its `in_use` place holds `{ grant_id, alloc_id,
    /// … }` and the connection `effect_config` is baked on its `t_request`
    /// effect transition. The grant_id is `<instance_id>:<loop_id>` (minted in
    /// `lower_loop`), so a held lease BELONGS to this instance iff its grant_id
    /// starts with `"<net_id>:"`. For each such hold we route a best-effort,
    /// idempotent `release_with_connection(effect_config, alloc_id)` through the
    /// installed allocator client (the same `ClusterRegistryAllocatorClient` the
    /// lease handlers use) — a cache HIT on `(resource_id, version)` reuses the
    /// already-built `ClusterClient` (SSH session intact; no secret
    /// re-resolution needed), `release_with_flavor` issues the `scancel` /
    /// `nomad job stop`, and the registry decrement arms idle-teardown when the
    /// cluster's active count hits 0 — freeing the watcher + SSH socket.
    ///
    /// ## Idempotency
    ///
    /// The marking is scanned ONCE; if the loop already released naturally there
    /// is no `in_use` hold to find → no-op. The allocator release is
    /// 404-tolerant, so a double-cancel (or a cancel racing a natural release)
    /// `scancel`s twice harmlessly.
    #[cfg(any(feature = "slurm", feature = "nomad"))]
    pub async fn release_held_leases_for_instance(&self, net_id: &str) {
        use petri_application::token_color_to_json;

        let Some(registry) = self.cluster_registry.read().clone() else {
            return; // no multi-cluster registry installed (plain http dev stack)
        };
        let allocator: Arc<dyn AllocatorClient> =
            Arc::new(crate::cluster_registry::ClusterRegistryAllocatorClient::new(registry));

        // grant_id is `<instance_id>:<loop_id>` where `<instance_id>` is the BARE
        // workflow-instance UUID (`loop_.rs`: `input._instance_id + ":<loop_id>"`),
        // NOT the engine net_id. The instance net_id is `mekhan-<uuid>`, so strip
        // the `mekhan-` prefix before matching — otherwise the prefix never
        // matches any held grant and the cancel leaks an orphan allocation.
        let instance_id = net_id.strip_prefix("mekhan-").unwrap_or(net_id);
        let grant_prefix = format!("{instance_id}:");

        // Scan every live lease-adapter pool-net for in_use holds owned by this
        // instance. The held alloc_id + the net's effect_config are all we need.
        let net_ids: Vec<String> = self.nets.read().keys().cloned().collect();
        for pool_net_id in net_ids {
            if !pool_net_id.starts_with("pool-") {
                continue;
            }
            let Some(instance) = self.get(&pool_net_id) else {
                continue;
            };

            // The connection effect_config is baked on the acquire effect
            // (`t_request`) — both lease transitions carry the SAME config.
            let Some(topology) = instance.service.get_topology() else {
                continue;
            };
            let effect_config = topology
                .get_transition(&petri_domain::TransitionId::named("t_request"))
                .and_then(|t| t.effect_config.clone());
            let Some(effect_config) = effect_config else {
                continue; // not a datacenter lease-adapter net (e.g. a token pool)
            };

            let marking = instance.service.get_marking().await;
            let in_use = petri_domain::PlaceId::named("in_use");
            for token in marking.tokens_at(&in_use) {
                let data = token_color_to_json(&token.color);
                let grant_id = data.get("grant_id").and_then(|v| v.as_str());
                let alloc_id = data.get("alloc_id").and_then(|v| v.as_str());
                let (Some(grant_id), Some(alloc_id)) = (grant_id, alloc_id) else {
                    continue;
                };
                if !grant_id.starts_with(&grant_prefix) {
                    continue; // a different instance's held lease
                }
                tracing::info!(
                    net_id = %net_id,
                    pool_net_id = %pool_net_id,
                    grant_id = %grant_id,
                    alloc_id = %alloc_id,
                    "cancel: releasing held cluster lease (forced scancel / job-stop)"
                );
                // Best-effort, idempotent. A cache hit reuses the built client;
                // a 404 (already gone) is tolerated by the allocator contract.
                if let Err(e) = allocator
                    .release_with_connection(&effect_config, alloc_id)
                    .await
                {
                    tracing::warn!(
                        net_id = %net_id,
                        grant_id = %grant_id,
                        alloc_id = %alloc_id,
                        error = %e,
                        "cancel: forced lease release failed (best-effort) — the allocator may \
                         already have reclaimed it, or the cluster client is not cached (cold \
                         engine); the salloc's own TTL / scancel-on-job-end is the backstop"
                    );
                }
            }
        }
    }

    /// Terminate a net: emit NetCancelled, cancel tasks, remove from memory.
    ///
    /// Returns `Err("Net '<id>' not found")` if no net with that id is
    /// currently registered (already terminal or never existed). Callers that
    /// need to distinguish "already gone" from a real error should treat the
    /// "not found" prefix as idempotent success — see
    /// [`RegistryCancellor`] for the canonical wrapper.
    pub async fn terminate(
        &self,
        net_id: &str,
        reason: Option<String>,
        cancelled_by: Option<String>,
    ) -> Result<(), String> {
        let instance = self
            .get(net_id)
            .ok_or_else(|| format!("Net '{}' not found", net_id))?;

        // Emit NetCancelled event
        let event = petri_domain::DomainEvent::NetCancelled {
            net_id: net_id.to_string(),
            reason,
            cancelled_by,
        };
        instance
            .service
            .append_event(event)
            .await
            .map_err(|e| e.to_string())?;

        // Cancel and remove
        self.hibernate(net_id)
    }
}

/// Spawn a background evaluation loop for a net instance.
fn spawn_net_evaluation_loop<E, T, S>(
    net_id: String,
    service: Arc<PetriNetService<E, T, S>>,
    adapter_scheduler: Arc<AdapterScheduler>,
    eval_notify: Arc<Notify>,
    run_mode: Arc<RwLock<RunMode>>,
    event_tx: Arc<broadcast::Sender<SseSignal>>,
    cancel_token: CancellationToken,
) where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    use petri_application::token_color_to_json;
    use petri_domain::{DomainEvent, PlaceId, Token, TokenColor};

    tokio::spawn(async move {
        // Track last broadcast sequence so we can catch ALL new events
        // (including those from NATS signal injection, adapter callbacks, etc.)
        let mut last_broadcast_seq: u64 = {
            let existing = service.get_events().await;
            existing.last().map_or(0, |e| e.sequence)
        };

        loop {
            tokio::select! {
                _ = cancel_token.cancelled() => {
                    tracing::info!(net_id = %net_id, "Eval loop cancelled, shutting down");
                    return;
                }
                _ = eval_notify.notified() => {}
            }

            // Only evaluate transitions when in Running mode
            let mode = *run_mode.read();
            if mode == RunMode::Running {
                match service.evaluate_until_quiescent(1000).await {
                    Ok(result) => {
                        if result.steps_executed > 0 {
                            tracing::info!(
                                net_id = %net_id,
                                "Auto-evaluated {} transitions, final state: {:?}",
                                result.steps_executed,
                                result.final_state
                            );

                            // Notify adapters about all produced tokens
                            for persisted in &result.events {
                                if let DomainEvent::TransitionFired {
                                    produced_tokens, ..
                                } = &persisted.event
                                {
                                    for (place_id, token) in produced_tokens {
                                        let token_data = token_color_to_json(&token.color);
                                        let token_created_at_ms =
                                            token.created_at.timestamp_millis();
                                        notify_adapters_in_eval_loop(
                                            &service,
                                            &adapter_scheduler,
                                            &eval_notify,
                                            &run_mode,
                                            place_id,
                                            token.id.clone(),
                                            token_data,
                                            token_created_at_ms,
                                        );
                                    }
                                }
                            }
                        }

                        // Check if net reached terminal state (completion tombstone)
                        if let Some(terminal) = &result.terminal_reached {
                            let completed_event = DomainEvent::NetCompleted {
                                net_id: net_id.clone(),
                                terminal_place_id: terminal.place_id.clone(),
                                exit_code: terminal.exit_code.clone(),
                            };
                            if let Err(e) = service.append_event(completed_event).await {
                                tracing::error!(
                                    net_id = %net_id,
                                    error = %e,
                                    "Failed to emit NetCompleted event"
                                );
                            }

                            // Broadcast the NetCompleted event to SSE before exiting
                            let all_events = service.get_events().await;
                            for event in &all_events {
                                if event.sequence > last_broadcast_seq {
                                    let _ =
                                        event_tx.send(SseSignal::Event(Box::new(event.clone())));
                                }
                            }

                            tracing::info!(
                                net_id = %net_id,
                                terminal_place = %terminal.place_id,
                                "Net completed — stopping eval loop"
                            );

                            // Cancel all per-net tasks (listeners, etc.)
                            cancel_token.cancel();
                            return;
                        }

                        // Check if a transition failed permanently. The firing
                        // layer already consumed the offending tokens and
                        // emitted the audit event (so the marking advanced and
                        // the loop would otherwise just quiesce). Raise a
                        // net-level NetFailed marker and tear the net down so
                        // the instance is unmistakably dead, not silently idle
                        // (mirrors the NetCompleted teardown above).
                        if let Some(failure) = &result.failure_reached {
                            let failed_event = DomainEvent::NetFailed {
                                net_id: net_id.clone(),
                                transition_id: failure.transition_id.clone(),
                                reason: failure.reason.clone(),
                                retryable: failure.retryable,
                            };
                            if let Err(e) = service.append_event(failed_event).await {
                                tracing::error!(
                                    net_id = %net_id,
                                    error = %e,
                                    "Failed to emit NetFailed event"
                                );
                            }

                            // Broadcast the NetFailed event to SSE before exiting
                            let all_events = service.get_events().await;
                            for event in &all_events {
                                if event.sequence > last_broadcast_seq {
                                    let _ =
                                        event_tx.send(SseSignal::Event(Box::new(event.clone())));
                                }
                            }
                            // Advance the cursor so the failure-bridge
                            // re-broadcast below doesn't re-send NetFailed.
                            if let Some(last) = all_events.last() {
                                last_broadcast_seq = last.sequence;
                            }

                            tracing::warn!(
                                net_id = %net_id,
                                transition = %failure.transition_id,
                                reason = %failure.reason,
                                "Net failed permanently — stopping eval loop"
                            );

                            // If this net was spawned as a child (SubWorkflow /
                            // agent tool), propagate the failure UP to the
                            // parent by bridging a failure token into the
                            // parent's failure_place — symmetric with the
                            // success reply bridge. The parent's SubWorkflow
                            // node consumes it (t_fail wired / t_fail_deadend
                            // unwired); an unwired deadend throws → the parent's
                            // OWN NetFailed → recurses up to the root. Root nets
                            // (no parent_net_id/failure_place params) take no
                            // branch here, so the success path and root
                            // lifecycle are untouched.
                            if let Some(params) = service.net_parameters() {
                                let parent = params.get("parent_net_id").and_then(|v| v.as_str());
                                let fplace = params.get("failure_place").and_then(|v| v.as_str());
                                if let (Some(parent_net_id), Some(failure_place)) = (parent, fplace)
                                {
                                    let payload = serde_json::json!({
                                        "reason":        failure.reason,
                                        // alias: agent tool-error Feedback codegen reads err.message
                                        "message":       failure.reason,
                                        "transition_id": failure.transition_id.to_string(),
                                        "child_net_id":  net_id,
                                        "retryable":     failure.retryable,
                                    });
                                    let bridge_event = DomainEvent::TokenBridgedOut {
                                        token: Token::new(TokenColor::Data(payload)),
                                        // audit-only: routing is driven by the
                                        // target_* fields, not the source.
                                        source_place_id: PlaceId::named("__net_failure"),
                                        source_place_name: "__net_failure".to_string(),
                                        target_net_id: parent_net_id.to_string(),
                                        target_place_name: failure_place.to_string(),
                                        transition_id: failure.transition_id.clone(),
                                        signal_key: uuid::Uuid::new_v4().to_string(),
                                        // synthetic teardown — no producing TransitionFired
                                        produced_by_event: None,
                                        // one-way bridge — the parent does not reply
                                        reply_to_place_name: None,
                                        reply_channels: None,
                                    };
                                    if let Err(e) = service.append_event(bridge_event).await {
                                        tracing::error!(
                                            net_id = %net_id,
                                            error = %e,
                                            "Failed to emit failure bridge to parent"
                                        );
                                    } else {
                                        // Broadcast the bridge event to SSE too.
                                        let all_events = service.get_events().await;
                                        for event in &all_events {
                                            if event.sequence > last_broadcast_seq {
                                                let _ = event_tx.send(SseSignal::Event(Box::new(
                                                    event.clone(),
                                                )));
                                            }
                                        }
                                    }
                                }
                            }

                            // Cancel all per-net tasks (listeners, etc.)
                            cancel_token.cancel();
                            return;
                        }
                    }
                    Err(e) => {
                        tracing::error!(net_id = %net_id, "Auto-evaluation error: {}", e);
                    }
                }
            }

            // Always broadcast new events to SSE clients regardless of run mode.
            // This catches events from any source: NATS signal injection,
            // HTTP handlers, adapter callbacks, eval results, etc.
            let all_events = service.get_events().await;
            for event in &all_events {
                if event.sequence > last_broadcast_seq {
                    let _ = event_tx.send(SseSignal::Event(Box::new(event.clone())));
                }
            }
            if let Some(last) = all_events.last() {
                last_broadcast_seq = last.sequence;
            }
        }
    });
}

/// Helper to notify adapters from within the evaluation loop.
#[allow(clippy::too_many_arguments)]
fn notify_adapters_in_eval_loop<E, T, S>(
    service: &Arc<PetriNetService<E, T, S>>,
    scheduler: &Arc<AdapterScheduler>,
    eval_notify: &Arc<Notify>,
    run_mode: &Arc<RwLock<RunMode>>,
    place_id: &petri_domain::PlaceId,
    token_id: petri_domain::TokenId,
    token_data: serde_json::Value,
    token_created_at_ms: i64,
) where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    let scheduler = scheduler.clone();
    let service = service.clone();
    let pid = place_id.clone();
    let eval_notify = eval_notify.clone();
    let run_mode = run_mode.clone();

    let inject_fn: Arc<dyn Fn(PlaceId, petri_domain::TokenColor) + Send + Sync> = {
        let svc = service.clone();
        let notify = eval_notify.clone();
        let mode = run_mode.clone();
        Arc::new(
            move |target_place: PlaceId, color: petri_domain::TokenColor| {
                let svc = svc.clone();
                let notify = notify.clone();
                let mode = mode.clone();
                tokio::spawn(async move {
                    let _ = svc.create_token(target_place, color).await;
                    if *mode.read() == RunMode::Running {
                        notify.notify_one();
                    }
                });
            },
        )
    };

    #[allow(clippy::type_complexity)]
    let check_token_fn: Arc<
        dyn Fn(&petri_domain::PlaceId, &petri_domain::TokenId) -> bool + Send + Sync,
    > = {
        let svc = service.clone();
        Arc::new(
            move |place_id: &petri_domain::PlaceId, token_id: &petri_domain::TokenId| {
                let svc = svc.clone();
                let pid = place_id.clone();
                let tid = token_id.clone();
                tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current()
                        .block_on(async move { svc.token_exists_in_place(&pid, &tid).await })
                })
            },
        )
    };

    scheduler.process_token_created(
        &pid,
        token_id,
        token_data,
        token_created_at_ms,
        inject_fn,
        check_token_fn,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_application::pre_dispatch::PreDispatchContext;
    use petri_test_harness::doubles::{
        MockEventRepository, MockStateProjection, MockTopologyRepository,
    };
    use std::sync::Arc;

    type MockRegistry =
        NetRegistry<MockEventRepository, MockTopologyRepository, MockStateProjection>;

    fn mock_store_factory(
    ) -> StoreFactory<MockEventRepository, MockTopologyRepository, MockStateProjection> {
        Arc::new(|_net_id: &str| {
            let (_tx, rx) = tokio::sync::watch::channel(0u64);
            (
                Arc::new(MockEventRepository::new()),
                Arc::new(MockTopologyRepository::new()),
                Arc::new(MockStateProjection::new()),
                rx,
            )
        })
    }

    fn new_registry() -> MockRegistry {
        NetRegistry::new(mock_store_factory())
    }

    #[tokio::test]
    async fn test_get_or_create_returns_same_instance() {
        let registry = new_registry();
        let inst1 = registry.get_or_create("net-1");
        let inst2 = registry.get_or_create("net-1");
        assert!(
            Arc::ptr_eq(&inst1, &inst2),
            "Same ID should return same Arc"
        );
    }

    #[tokio::test]
    async fn test_get_or_create_different_ids() {
        let registry = new_registry();
        let inst1 = registry.get_or_create("net-1");
        let inst2 = registry.get_or_create("net-2");
        assert!(
            !Arc::ptr_eq(&inst1, &inst2),
            "Different IDs should return different instances"
        );
    }

    #[test]
    fn test_get_returns_none_for_unknown() {
        let registry = new_registry();
        assert!(registry.get("nonexistent").is_none());
    }

    #[tokio::test]
    async fn test_hibernate_cancels_token() {
        let registry = new_registry();
        let inst = registry.get_or_create("net-1");
        let cancel = inst.cancel_token.clone();

        assert!(!cancel.is_cancelled(), "Should not be cancelled initially");
        registry
            .hibernate("net-1")
            .expect("hibernate should succeed");
        assert!(
            cancel.is_cancelled(),
            "Cancel token should be cancelled after hibernate"
        );
    }

    #[tokio::test]
    async fn test_hibernate_removes_from_registry() {
        let registry = new_registry();
        registry.get_or_create("net-1");
        registry
            .hibernate("net-1")
            .expect("hibernate should succeed");
        assert!(
            registry.get("net-1").is_none(),
            "Net should be removed after hibernate"
        );
    }

    #[test]
    fn test_hibernate_unknown_net_errors() {
        let registry = new_registry();
        let result = registry.hibernate("nonexistent");
        assert!(result.is_err(), "Hibernate should fail for unknown net");
        assert!(result.unwrap_err().contains("not found"));
    }

    #[tokio::test]
    async fn test_terminate_emits_net_cancelled() {
        let registry = new_registry();
        let inst = registry.get_or_create("net-1");

        // Initialize so the service has a topology
        inst.service
            .initialize(petri_domain::PetriNet::new())
            .await
            .unwrap();

        registry
            .terminate("net-1", Some("test reason".into()), Some("admin".into()))
            .await
            .expect("terminate should succeed");

        // Instance was removed from registry
        assert!(registry.get("net-1").is_none());

        // Check event was emitted (inst still holds Arc to old service)
        let events = inst.service.get_events().await;
        let has_cancelled = events.iter().any(|e| {
            matches!(
                &e.event,
                petri_domain::DomainEvent::NetCancelled { net_id, reason, cancelled_by }
                    if net_id == "net-1"
                    && reason.as_deref() == Some("test reason")
                    && cancelled_by.as_deref() == Some("admin")
            )
        });
        assert!(has_cancelled, "Should have NetCancelled event");
    }

    #[tokio::test]
    async fn test_terminate_unknown_net_errors() {
        let registry = new_registry();
        let result = registry.terminate("nonexistent", None, None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_after_hibernate() {
        let registry = new_registry();
        registry.get_or_create("net-1");
        registry.get_or_create("net-2");
        registry.get_or_create("net-3");

        assert_eq!(registry.list().len(), 3);

        registry.hibernate("net-2").unwrap();

        let remaining = registry.list();
        assert_eq!(remaining.len(), 2);
        assert!(!remaining.contains(&"net-2".to_string()));
    }

    #[tokio::test]
    async fn test_cancel_token_initially_not_cancelled() {
        let registry = new_registry();
        let inst = registry.get_or_create("net-1");
        assert!(!inst.cancel_token.is_cancelled());
    }

    #[tokio::test]
    async fn test_eval_loop_stops_on_cancel() {
        let registry = new_registry();
        let inst = registry.get_or_create("net-1");
        let cancel = inst.cancel_token.clone();

        // Cancel the token
        cancel.cancel();

        // The eval loop task should finish within a reasonable timeout.
        // We can't directly observe the spawned task, but we can verify
        // the cancel_token is cancelled (the loop checks this in tokio::select!).
        assert!(cancel.is_cancelled());

        // Give tokio a chance to process the cancellation
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn test_eval_loop_emits_net_completed_on_terminal() {
        use petri_test_harness::fixtures::TestScenario;

        let registry = new_registry();
        let inst = registry.get_or_create("net-1");

        // Load terminal scenario: [Input] → (Process) → [Done:Terminal]
        let scenario = TestScenario::with_terminal(Some(serde_json::json!(42)));
        inst.service
            .initialize(scenario.net)
            .await
            .expect("initialize");
        for (place_id, token) in &scenario.initial_tokens {
            inst.service
                .create_token(place_id.clone(), token.color.clone())
                .await
                .expect("create token");
        }

        // Switch to Running mode and trigger evaluation
        *inst.run_mode.write() = RunMode::Running;
        inst.eval_notify.notify_one();

        // Wait for eval loop to process (with generous timeout)
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let events = inst.service.get_events().await;
            let has_completed = events
                .iter()
                .any(|e| matches!(&e.event, petri_domain::DomainEvent::NetCompleted { .. }));
            if has_completed {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "Timed out waiting for NetCompleted event. Events: {:?}",
                    events
                        .iter()
                        .map(|e| format!("{:?}", e.event))
                        .collect::<Vec<_>>()
                );
            }
        }

        // Verify the NetCompleted event has correct fields
        let events = inst.service.get_events().await;
        let completed = events
            .iter()
            .find(|e| matches!(&e.event, petri_domain::DomainEvent::NetCompleted { .. }))
            .expect("NetCompleted event should exist");

        match &completed.event {
            petri_domain::DomainEvent::NetCompleted {
                net_id,
                terminal_place_id,
                exit_code,
            } => {
                assert_eq!(net_id, "net-1");
                assert_eq!(
                    terminal_place_id,
                    &scenario.places["Done"].to_string(),
                    "Should report Done as the terminal place"
                );
                assert_eq!(
                    *exit_code,
                    Some(serde_json::json!(42)),
                    "Should carry the exit_code from the token"
                );
            }
            _ => unreachable!(),
        }
    }

    /// A net spawned as a child (net_parameters carry `parent_net_id` +
    /// `failure_place`) must, on permanent NetFailed, emit a TokenBridgedOut
    /// routing a failure token to the parent's failure_place — symmetric with
    /// the success reply bridge. This is what wakes a stuck SubWorkflow/agent
    /// parent instead of leaving it waiting on its reply bridge forever.
    #[tokio::test]
    async fn test_eval_loop_emits_failure_bridge_on_net_failed() {
        use petri_test_harness::fixtures::TestScenario;

        let registry = new_registry();
        let inst = registry.get_or_create("child-1");

        // A net whose single transition fails permanently (undefined var →
        // permanent ScriptError → failure_reached).
        let scenario = TestScenario::with_failing_transition();

        inst.service
            .initialize(scenario.net)
            .await
            .expect("initialize");
        for (place_id, token) in &scenario.initial_tokens {
            inst.service
                .create_token(place_id.clone(), token.color.clone())
                .await
                .expect("create token");
        }

        // Mark this net as a SubWorkflow child of "parent-xyz".
        inst.service.set_net_parameters(serde_json::json!({
            "parent_net_id": "parent-xyz",
            "failure_place": "p_sub_failure",
        }));

        *inst.run_mode.write() = RunMode::Running;
        inst.eval_notify.notify_one();

        // Wait for NetFailed to land.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let events = inst.service.get_events().await;
            if events
                .iter()
                .any(|e| matches!(&e.event, petri_domain::DomainEvent::NetFailed { .. }))
            {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!(
                    "Timed out waiting for NetFailed. Events: {:?}",
                    events
                        .iter()
                        .map(|e| format!("{:?}", e.event))
                        .collect::<Vec<_>>()
                );
            }
        }

        // A failure-bridge TokenBridgedOut must target the parent's failure_place.
        let events = inst.service.get_events().await;
        let bridged = events
            .iter()
            .find_map(|e| match &e.event {
                petri_domain::DomainEvent::TokenBridgedOut {
                    token,
                    target_net_id,
                    target_place_name,
                    ..
                } if target_net_id == "parent-xyz" && target_place_name == "p_sub_failure" => {
                    Some(token.clone())
                }
                _ => None,
            })
            .expect("a failure-bridge TokenBridgedOut to the parent should exist");

        match &bridged.color {
            petri_domain::TokenColor::Data(v) => {
                assert!(
                    v.get("reason").and_then(|r| r.as_str()).is_some(),
                    "failure payload must carry a `reason` string, got {v:?}"
                );
                assert!(
                    v.get("message").and_then(|r| r.as_str()).is_some(),
                    "failure payload must carry a `message` alias (agent codegen reads err.message), got {v:?}"
                );
                assert_eq!(
                    v.get("child_net_id").and_then(|r| r.as_str()),
                    Some("child-1"),
                    "failure payload must name the failing child net"
                );
                assert!(
                    v.get("retryable").map(|r| r.is_boolean()).unwrap_or(false),
                    "failure payload must carry a `retryable` bool, got {v:?}"
                );
            }
            other => panic!("expected TokenColor::Data failure payload, got {other:?}"),
        }
    }

    /// A ROOT net (no parent_net_id/failure_place params) must NOT bridge any
    /// failure on NetFailed — only NetFailed itself. Guards the (Some,Some)
    /// gate so the success path and root lifecycle stay untouched.
    #[tokio::test]
    async fn test_eval_loop_no_failure_bridge_for_root_net() {
        use petri_test_harness::fixtures::TestScenario;

        let registry = new_registry();
        let inst = registry.get_or_create("root-1");

        let scenario = TestScenario::with_failing_transition();

        inst.service
            .initialize(scenario.net)
            .await
            .expect("initialize");
        for (place_id, token) in &scenario.initial_tokens {
            inst.service
                .create_token(place_id.clone(), token.color.clone())
                .await
                .expect("create token");
        }

        // NO set_net_parameters → this is a root net.

        *inst.run_mode.write() = RunMode::Running;
        inst.eval_notify.notify_one();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let events = inst.service.get_events().await;
            if events
                .iter()
                .any(|e| matches!(&e.event, petri_domain::DomainEvent::NetFailed { .. }))
            {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("Timed out waiting for NetFailed on root net");
            }
        }

        let events = inst.service.get_events().await;
        let has_bridge = events
            .iter()
            .any(|e| matches!(&e.event, petri_domain::DomainEvent::TokenBridgedOut { .. }));
        assert!(
            !has_bridge,
            "a root net (no parent params) must NOT emit a failure bridge on NetFailed"
        );
    }

    #[tokio::test]
    async fn test_eval_loop_cancels_token_on_terminal() {
        use petri_test_harness::fixtures::TestScenario;

        let registry = new_registry();
        let inst = registry.get_or_create("net-1");
        let cancel = inst.cancel_token.clone();

        // Load terminal scenario
        let scenario = TestScenario::with_terminal(None);
        inst.service
            .initialize(scenario.net)
            .await
            .expect("initialize");
        for (place_id, token) in &scenario.initial_tokens {
            inst.service
                .create_token(place_id.clone(), token.color.clone())
                .await
                .expect("create token");
        }

        assert!(
            !cancel.is_cancelled(),
            "Should not be cancelled before eval"
        );

        // Run eval
        *inst.run_mode.write() = RunMode::Running;
        inst.eval_notify.notify_one();

        // Wait for cancel_token to be cancelled
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if cancel.is_cancelled() {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                panic!("Timed out waiting for cancel_token to be cancelled after terminal");
            }
        }

        assert!(
            cancel.is_cancelled(),
            "Cancel token should be cancelled after terminal completion"
        );
    }

    #[tokio::test]
    async fn test_eval_loop_no_net_completed_without_terminal() {
        use petri_test_harness::fixtures::TestScenario;

        let registry = new_registry();
        let inst = registry.get_or_create("net-1");

        // Load simple pass-through (no terminal places)
        let scenario = TestScenario::simple_pass_through();
        inst.service
            .initialize(scenario.net)
            .await
            .expect("initialize");
        for (place_id, token) in &scenario.initial_tokens {
            inst.service
                .create_token(place_id.clone(), token.color.clone())
                .await
                .expect("create token");
        }

        // Run eval
        *inst.run_mode.write() = RunMode::Running;
        inst.eval_notify.notify_one();

        // Wait for transitions to fire
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Check no NetCompleted event
        let events = inst.service.get_events().await;
        let has_completed = events
            .iter()
            .any(|e| matches!(&e.event, petri_domain::DomainEvent::NetCompleted { .. }));
        assert!(
            !has_completed,
            "Should NOT emit NetCompleted for non-terminal scenario"
        );

        // Cancel token should still be active
        assert!(!inst.cancel_token.is_cancelled());
    }

    /// Regression test: concurrent HTTP evaluate + eval loop must not double-fire
    /// an effect transition on the same token.
    ///
    /// Reproduces the bug where `POST /command/evaluate` and the background eval
    /// loop both call `evaluate_until_quiescent` on the same service, causing an
    /// effect to execute twice for a single input token.
    #[tokio::test]
    async fn test_concurrent_evaluate_does_not_double_fire_effect() {
        use petri_application::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};
        use petri_domain::{Place, Port, TokenColor, Transition};
        use petri_test_harness::prelude::PetriArc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // -- Mock effect handler that counts executions and adds a small delay --
        struct CountingEffectHandler {
            count: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl EffectHandler for CountingEffectHandler {
            async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
                self.count.fetch_add(1, Ordering::SeqCst);
                // Small delay to widen the race window
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                let inp = input
                    .inputs
                    .get("inp")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                let mut tokens = std::collections::HashMap::new();
                tokens.insert("out".to_string(), inp);
                Ok(EffectOutput {
                    tokens,
                    result: serde_json::json!({"fired": true}),
                })
            }

            fn name(&self) -> &str {
                "counting"
            }
        }

        // -- Build a simple net: [input] --(effect "counting")--> [output] --
        let mut net = petri_domain::PetriNet::new();
        let input = Place::internal("input");
        let output = Place::internal("output");
        let transition = Transition::new("do_effect", "")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out")])
            .with_effect_handler("counting");

        let input_id = input.id.clone();
        let output_id = output.id.clone();
        let transition_id = transition.id.clone();

        net.add_place(input);
        net.add_place(output);
        net.add_transition(transition);
        net.add_arc(PetriArc::input(
            input_id.clone(),
            transition_id.clone(),
            "inp",
        ));
        net.add_arc(PetriArc::output(transition_id, "out", output_id.clone()));

        // -- Set up registry + instance --
        let registry = new_registry();
        let inst = registry.get_or_create("race-test");

        let execute_count = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingEffectHandler {
            count: execute_count.clone(),
        });

        inst.service
            .register_effect_handler("counting", handler)
            .expect("register handler");
        inst.service.initialize(net).await.expect("initialize");

        // Seed ONE token
        inst.service
            .create_token(
                input_id.clone(),
                TokenColor::Data(serde_json::json!({"job": 1})),
            )
            .await
            .expect("create token");

        // -- Activate the eval loop (Running mode) --
        *inst.run_mode.write() = RunMode::Running;

        // -- Trigger the race: notify eval loop AND call evaluate concurrently --
        inst.eval_notify.notify_one();
        let svc = inst.service.clone();
        let concurrent_eval = tokio::spawn(async move { svc.evaluate_until_quiescent(100).await });

        // Let both paths run
        let _ = concurrent_eval.await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // -- Assert: the effect must fire exactly once --
        let count = execute_count.load(Ordering::SeqCst);
        assert_eq!(
            count, 1,
            "Effect should fire exactly once, but fired {} times. \
             This indicates a race between HTTP evaluate and the eval loop.",
            count
        );

        // Verify exactly one EffectCompleted event
        let events = inst.service.get_events().await;
        let effect_completed_count = events
            .iter()
            .filter(|e| matches!(&e.event, petri_domain::DomainEvent::EffectCompleted { .. }))
            .count();
        assert_eq!(
            effect_completed_count, 1,
            "Should have exactly 1 EffectCompleted event, found {}",
            effect_completed_count
        );
    }

    /// Confirms that the service-level eval lock prevents double-firing even
    /// when evaluate_until_quiescent is called directly (bypassing the HTTP
    /// handler's run_mode guard). The concurrent call returns 0 steps.
    #[tokio::test]
    async fn test_eval_lock_prevents_concurrent_evaluate() {
        use petri_application::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};
        use petri_domain::{Place, Port, TokenColor, Transition};
        use petri_test_harness::prelude::PetriArc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingEffectHandler {
            count: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl EffectHandler for CountingEffectHandler {
            async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
                self.count.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                let inp = input
                    .inputs
                    .get("inp")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                let mut tokens = std::collections::HashMap::new();
                tokens.insert("out".to_string(), inp);
                Ok(EffectOutput {
                    tokens,
                    result: serde_json::json!({"fired": true}),
                })
            }

            fn name(&self) -> &str {
                "counting"
            }
        }

        let mut net = petri_domain::PetriNet::new();
        let input = Place::internal("input");
        let output = Place::internal("output");
        let transition = Transition::new("do_effect", "")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out")])
            .with_effect_handler("counting");

        let input_id = input.id.clone();
        let output_id = output.id.clone();
        let transition_id = transition.id.clone();

        net.add_place(input);
        net.add_place(output);
        net.add_transition(transition);
        net.add_arc(PetriArc::input(
            input_id.clone(),
            transition_id.clone(),
            "inp",
        ));
        net.add_arc(PetriArc::output(transition_id, "out", output_id.clone()));

        let registry = new_registry();
        let inst = registry.get_or_create("race-test-locked");

        let execute_count = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingEffectHandler {
            count: execute_count.clone(),
        });

        inst.service
            .register_effect_handler("counting", handler)
            .expect("register handler");
        inst.service.initialize(net).await.expect("initialize");
        inst.service
            .create_token(
                input_id.clone(),
                TokenColor::Data(serde_json::json!({"job": 1})),
            )
            .await
            .expect("create token");

        *inst.run_mode.write() = RunMode::Running;

        // Call evaluate directly while the eval loop is also active.
        // The service-level eval_lock should prevent double-firing.
        inst.eval_notify.notify_one();
        let svc = inst.service.clone();
        let http_eval = tokio::spawn(async move { svc.evaluate_until_quiescent(100).await });

        let _ = http_eval.await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let count = execute_count.load(Ordering::SeqCst);
        assert_eq!(
            count, 1,
            "Eval lock should prevent double-firing. Effect fired {} times.",
            count
        );
    }

    // ========================================================================
    // Pre-dispatch hook registry tests (spec § 6 / § 11 trip-wire 7).
    // ========================================================================

    struct NoopHook {
        name: String,
    }

    impl NoopHook {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl PreDispatchHook for NoopHook {
        async fn pre_dispatch(
            &self,
            _ctx: &PreDispatchContext<'_>,
        ) -> Result<
            petri_application::pre_dispatch::PreDispatchOutcome,
            petri_application::pre_dispatch::PreDispatchError,
        > {
            Ok(
                petri_application::pre_dispatch::PreDispatchOutcome::Continue {
                    enriched_effect_config: None,
                },
            )
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    #[tokio::test]
    async fn test_register_pre_dispatch_hook_before_freeze_succeeds() {
        let registry = new_registry();
        assert!(!registry.pre_dispatch_is_frozen());
        registry
            .register_pre_dispatch_hook("noop", Arc::new(NoopHook::new("noop")))
            .expect("registration before first net should succeed");
    }

    #[tokio::test]
    async fn test_register_pre_dispatch_hook_after_freeze_errors() {
        let registry = new_registry();
        // Instantiate a net to flip the frozen flag.
        let _inst = registry.get_or_create("net-frozen-1");
        assert!(registry.pre_dispatch_is_frozen());
        let result = registry.register_pre_dispatch_hook("noop", Arc::new(NoopHook::new("noop")));
        match result {
            Err(RegistrationError::RegistryFrozen(name)) => assert_eq!(name, "noop"),
            other => panic!("expected RegistryFrozen, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_register_pre_dispatch_hook_duplicate_name_errors() {
        let registry = new_registry();
        registry
            .register_pre_dispatch_hook("noop", Arc::new(NoopHook::new("noop")))
            .expect("first registration should succeed");
        let result = registry.register_pre_dispatch_hook("noop", Arc::new(NoopHook::new("noop")));
        match result {
            Err(RegistrationError::DuplicateName(name)) => assert_eq!(name, "noop"),
            other => panic!("expected DuplicateName, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_set_chain_configs_after_freeze_errors() {
        let registry = new_registry();
        let _inst = registry.get_or_create("net-frozen-2");
        let result = registry.set_pre_dispatch_chain_configs(vec![]);
        assert!(matches!(result, Err(RegistrationError::RegistryFrozen(_))));
    }

    #[tokio::test]
    async fn test_chain_assembled_in_declaration_order_with_builtin_resolution() {
        let registry = new_registry();
        registry
            .register_pre_dispatch_hook("h1", Arc::new(NoopHook::new("h1")))
            .unwrap();
        registry
            .register_pre_dispatch_hook("h2", Arc::new(NoopHook::new("h2")))
            .unwrap();
        registry
            .set_pre_dispatch_chain_configs(vec![
                PreDispatchHookConfig {
                    name: "h1".to_string(),
                    transport: PreDispatchTransport::Builtin,
                    fail_open: false,
                    timeout_ms: 500,
                    url: None,
                    match_effect_handlers: vec![],
                    http_max_retries: 0,
                },
                PreDispatchHookConfig {
                    name: "h2".to_string(),
                    transport: PreDispatchTransport::Builtin,
                    fail_open: true,
                    timeout_ms: 500,
                    url: None,
                    match_effect_handlers: vec![],
                    http_max_retries: 0,
                },
            ])
            .unwrap();
        let inst = registry.get_or_create("net-chain-asm");
        let rt = inst
            .service
            .pre_dispatch_runtime()
            .expect("net instance must have runtime bound");
        assert_eq!(rt.chain.len(), 2);
        assert_eq!(rt.chain.entries[0].hook.name(), "h1");
        assert_eq!(rt.chain.entries[1].hook.name(), "h2");
        assert!(!rt.chain.entries[0].fail_open);
        assert!(rt.chain.entries[1].fail_open);
    }
}

// ---------------------------------------------------------------------------
// NatsCatalogueClient — NATS request-reply for catalogue queries
// ---------------------------------------------------------------------------

#[cfg(feature = "catalogue")]
mod nats_catalogue_client {
    use petri_domain::catalogue::{
        CatalogueClient, CatalogueError, CatalogueLookupRequest, CatalogueLookupResponse,
        CatalogueSubscribeRequest,
    };

    /// NATS implementation of the `CatalogueClient` trait.
    ///
    /// Uses core NATS request-reply for synchronous queries (lookup,
    /// subscribe, unsubscribe). Catalogue registration is handled by the
    /// causality projector in Mekhan via PETRI_GLOBAL domain events.
    pub struct NatsCatalogueClient {
        client: async_nats::Client,
    }

    impl NatsCatalogueClient {
        pub fn new(client: async_nats::Client) -> Self {
            Self { client }
        }
    }

    #[async_trait::async_trait]
    impl CatalogueClient for NatsCatalogueClient {
        async fn lookup(
            &self,
            request: CatalogueLookupRequest,
        ) -> Result<CatalogueLookupResponse, CatalogueError> {
            let payload = serde_json::to_vec(&request)
                .map_err(|e| CatalogueError::QueryFailed(e.to_string()))?;

            let response = self
                .client
                .request("catalogue.query.list", payload.into())
                .await
                .map_err(|e| CatalogueError::QueryFailed(format!("NATS request failed: {e}")))?;

            // The Mekhan responder wraps results in CatalogueResponse { data, error }
            let wrapper: serde_json::Value = serde_json::from_slice(&response.payload)
                .map_err(|e| CatalogueError::QueryFailed(format!("response parse failed: {e}")))?;

            // Check for error field
            if let Some(err) = wrapper.get("error").and_then(|e| e.as_str()) {
                return Err(CatalogueError::QueryFailed(err.to_string()));
            }

            // Extract data field (Mekhan wraps in CatalogueResponse { data, error })
            let data = wrapper.get("data").ok_or_else(|| {
                CatalogueError::QueryFailed("missing data field in response".into())
            })?;

            let parsed: CatalogueLookupResponse =
                serde_json::from_value(data.clone()).map_err(|e| {
                    CatalogueError::QueryFailed(format!("response data parse failed: {e}"))
                })?;

            Ok(parsed)
        }

        async fn subscribe(
            &self,
            request: CatalogueSubscribeRequest,
        ) -> Result<String, CatalogueError> {
            let payload = serde_json::to_vec(&request)
                .map_err(|e| CatalogueError::QueryFailed(e.to_string()))?;

            let response = self
                .client
                .request("catalogue.subscribe", payload.into())
                .await
                .map_err(|e| {
                    CatalogueError::QueryFailed(format!("NATS subscribe request failed: {e}"))
                })?;

            let wrapper: serde_json::Value =
                serde_json::from_slice(&response.payload).map_err(|e| {
                    CatalogueError::QueryFailed(format!("subscribe response parse failed: {e}"))
                })?;

            if let Some(err) = wrapper.get("error").and_then(|e| e.as_str()) {
                return Err(CatalogueError::QueryFailed(err.to_string()));
            }

            let subscription_id = wrapper
                .get("data")
                .and_then(|d| d.get("subscription_id"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    CatalogueError::QueryFailed("missing subscription_id in response".into())
                })?
                .to_string();

            Ok(subscription_id)
        }

        async fn unsubscribe(&self, subscription_id: &str) -> Result<bool, CatalogueError> {
            let payload =
                serde_json::to_vec(&serde_json::json!({ "subscription_id": subscription_id }))
                    .map_err(|e| CatalogueError::QueryFailed(e.to_string()))?;

            let response = self
                .client
                .request("catalogue.unsubscribe", payload.into())
                .await
                .map_err(|e| {
                    CatalogueError::QueryFailed(format!("NATS unsubscribe request failed: {e}"))
                })?;

            let wrapper: serde_json::Value =
                serde_json::from_slice(&response.payload).map_err(|e| {
                    CatalogueError::QueryFailed(format!("unsubscribe response parse failed: {e}"))
                })?;

            if let Some(err) = wrapper.get("error").and_then(|e| e.as_str()) {
                return Err(CatalogueError::QueryFailed(err.to_string()));
            }

            let unsubscribed = wrapper
                .get("data")
                .and_then(|d| d.get("unsubscribed"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            Ok(unsubscribed)
        }

        fn name(&self) -> &str {
            "nats-catalogue"
        }
    }
}

#[cfg(feature = "catalogue")]
use nats_catalogue_client::NatsCatalogueClient;

// ---------------------------------------------------------------------------
// NetTopologyResolver impl — bridges application-layer bridge validation
// to the API-layer net registry.
// ---------------------------------------------------------------------------

/// Adapter that bridges `SubWorkflowCancellor` (defined in petri-domain)
/// onto `NetRegistry::terminate`. Construct via [`RegistryCancellor::new`]
/// after the registry is `Arc`-wrapped; install on the registry with
/// [`NetRegistry::set_subworkflow_cancellor`] so the `subworkflow_cancel`
/// handler can terminate child nets without `application` depending on `api`.
pub struct RegistryCancellor<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    registry: Arc<NetRegistry<E, T, S>>,
}

impl<E, T, S> RegistryCancellor<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    pub fn new(registry: Arc<NetRegistry<E, T, S>>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl<E, T, S> SubWorkflowCancellor for RegistryCancellor<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    async fn cancel(
        &self,
        request: petri_domain::subworkflow::SubWorkflowCancelRequest,
    ) -> Result<bool, petri_domain::subworkflow::SubWorkflowCancelError> {
        match self
            .registry
            .terminate(
                &request.child_net_id,
                request.reason.clone(),
                Some("subworkflow_cancel".to_string()),
            )
            .await
        {
            Ok(()) => Ok(true),
            Err(e) if e.starts_with("Net '") && e.ends_with("' not found") => Ok(false),
            Err(e) => Err(petri_domain::subworkflow::SubWorkflowCancelError::CancellationFailed(e)),
        }
    }

    fn name(&self) -> &str {
        "net-registry-cancellor"
    }
}

impl<E, T, S> petri_application::NetTopologyResolver for NetRegistry<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    fn resolve_topology(&self, net_id: &str) -> Option<petri_domain::PetriNet> {
        self.get(net_id)
            .and_then(|inst| inst.service.get_topology())
    }

    fn all_net_ids(&self) -> Vec<String> {
        self.list()
    }
}
