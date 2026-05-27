mod config;

use std::sync::Arc;

use petri_api::HumanIntegrationConfig;
use petri_api::{create_router_with_registry, NetRegistry};
use petri_infrastructure::{MarkingProjection, MemoryEventStore, MemoryTopologyStore};
use petri_nats::human_client::HumanNatsClient;
use petri_nats::GlobalHumanResultListener;
use petri_nats::{
    ActivityTracker, Clockmaster, CreateNetListener, EventConsumer,
    GlobalBridgeListener, GlobalSignalListener, HibernationMaster, NatsConfig, NatsEventStore,
    NatsTimerClient, NetMetadataProjection, ACTIVITY_KV_BUCKET, METADATA_KV_BUCKET,
};
use petri_nats::Subjects;
use petri_nats::{NetMetadata, NetStatus};
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::EngineConfig;

#[tokio::main]
async fn main() {
    // Initialize tracing (override with RUST_LOG env var)
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,petri_application=debug,petri_api=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Tracing initialized");

    let engine_config = EngineConfig::from_env();

    let config = NatsConfig::from_env();
    info!(url = %config.url, "Connecting to NATS");

    let jetstream = config
        .connect_jetstream()
        .await
        .expect("NATS connection is required — set NATS_URL or check that the server is reachable");
    info!("Connected to NATS JetStream");

    // Ensure the streams exist
    if let Err(e) = ensure_streams(&jetstream).await {
        tracing::warn!(error = %e, "Failed to create NATS streams (they may already exist)");
    }

    // Ensure the timers KV bucket exists
    if let Err(e) = ensure_timer_kv(&jetstream).await {
        tracing::warn!(error = %e, "Failed to create timers KV bucket");
    }

    // Initialize timer client and clockmaster
    let timer_client = match NatsTimerClient::new(&jetstream).await {
        Ok(client) => Some(Arc::new(client)),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to create NatsTimerClient, timers disabled");
            None
        }
    };

    if let Ok(clockmaster) = Clockmaster::new(jetstream.clone()).await {
        info!("Starting Clockmaster service");
        tokio::spawn(async move {
            if let Err(e) = clockmaster.run().await {
                tracing::error!(error = %e, "Clockmaster service stopped with error");
            }
        });
    }

    // Create lifecycle KV buckets (activity tracking + metadata)
    let activity_kv = ensure_lifecycle_kv(&jetstream, ACTIVITY_KV_BUCKET).await;
    let metadata_kv = ensure_lifecycle_kv(&jetstream, METADATA_KV_BUCKET).await;

    // Initialize activity tracker for hibernation
    let idle_timeout = std::time::Duration::from_secs(
        std::env::var("PETRI_IDLE_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300), // 5 minutes default
    );
    let activity_tracker = activity_kv.map(|kv| Arc::new(ActivityTracker::new(kv, idle_timeout)));

    // Clone metadata KV for the resolver (tombstone check on signal routing)
    let metadata_kv_for_resolver = metadata_kv.as_ref().cloned();

    // Clone metadata KV for the API discovery endpoint (before projection consumes it)
    let metadata_kv_for_api = metadata_kv.as_ref().cloned();

    // Clone metadata KV for the registry so control-plane handlers can rehydrate
    // hibernated nets after a cold engine boot.
    let metadata_kv_for_registry = metadata_kv.as_ref().cloned();

    // Start net metadata projection
    if let Some(kv) = metadata_kv {
        let projection = NetMetadataProjection::new(jetstream.clone(), kv);
        info!("Starting net metadata projection");
        let _handle = projection.start();
    }

    engine_config.print_startup_banner();

    // Store factory: each net gets a NatsEventStore backed by a consumer
    let js = jetstream.clone();
    let cfg = config.clone();
    let shutdown_token = tokio_util::sync::CancellationToken::new();
    let shutdown_for_factory = shutdown_token.clone();

    let store_factory: petri_api::net_registry::StoreFactory<
        NatsEventStore<MemoryEventStore>,
        MemoryTopologyStore,
        MarkingProjection,
    > = Arc::new(move |net_id: &str| {
        let cache = Arc::new(MemoryEventStore::new());
        let topology_store = Arc::new(MemoryTopologyStore::new());

        // Create watch channel for consumer → writer synchronization
        let (applied_tx, applied_rx) = tokio::sync::watch::channel(0u64);

        // Create oneshot for hydration-complete signal
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();

        // Create and start consumer (handles hydration + live consumption)
        let consumer = EventConsumer::new(
            cache.clone(),
            topology_store.clone(),
            applied_tx,
            ready_tx,
        );

        let js_consumer = js.clone();
        let net_id_consumer = net_id.to_string();
        let shutdown = shutdown_for_factory.clone();

        // Use block_in_place to start consumer and wait for hydration
        tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                // Spawn consumer as background task
                tokio::spawn(async move {
                    if let Err(e) = consumer
                        .start(&js_consumer, &net_id_consumer, shutdown)
                        .await
                    {
                        tracing::error!(
                            error = %e,
                            net_id = %net_id_consumer,
                            "Event consumer stopped with error"
                        );
                    }
                });

                // Wait for hydration to complete before returning stores
                if ready_rx.await.is_err() {
                    tracing::warn!("Event consumer ready signal dropped (consumer may have failed)");
                }
            })
        });

        // Each net gets its own config with the correct net_id for bridge routing
        let mut net_cfg = cfg.clone();
        net_cfg.net_id = Some(net_id.to_string());
        let applied_rx_for_registry = applied_rx.clone();
        let event_store = Arc::new(NatsEventStore::new(cache, js.clone(), net_cfg, applied_rx));
        (
            event_store,
            topology_store,
            Arc::new(MarkingProjection::new()),
            applied_rx_for_registry,
        )
    });

    let mut registry = NetRegistry::new(store_factory);
    if let Some(kv) = metadata_kv_for_registry {
        registry.set_metadata_lookup(Arc::new(KvMetadataLookup { metadata_kv: kv }));
    }
    if let Some(cfg) = engine_config.build_scheduler_config() {
        registry.set_scheduler_config(cfg);
    }
    if let Some(client) = timer_client {
        registry.set_timer_client(client);
    }

    let hook_configs = engine_config
        .load_pre_dispatch_hooks()
        .expect("failed to load pre-dispatch hooks config");
    registry
        .set_pre_dispatch_chain_configs(hook_configs)
        .expect("failed to set pre-dispatch chain configs at startup");

    // Human Task Integration
    {
        // Create a factory that produces per-net human clients with the correct net_id
        let js_for_human = jetstream.clone();
        let human_org_id = engine_config.human_org_id.clone();
        registry.set_human_config(HumanIntegrationConfig {
            client_factory: Arc::new(move |net_id: &str| {
                Arc::new(HumanNatsClient::new(
                    js_for_human.clone(),
                    net_id.to_string(),
                    human_org_id.clone(),
                )) as Arc<dyn petri_domain::human::HumanTaskClient>
            }),
        });
    }

    // Connect executor NATS client and set config on registry (behind feature gate)
    #[cfg(feature = "executor")]
    let executor_nats_client = if engine_config.is_executor_enabled() {
        let executor_nats_url = std::env::var("EXECUTOR_NATS_URL")
            .or_else(|_| std::env::var("NATS_URL"))
            .unwrap_or_else(|_| "nats://localhost:4333".to_string());

        // Build executor NATS options using the same tuning as the main connection
        // (ping_interval, connection_timeout, event_callback) but with its own name.
        let mut executor_config = config.clone();
        executor_config.connection_name = "petri-executor".to_string();
        let options = match executor_config.build_options().await {
            Ok(opts) => opts,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to build executor NATS options, falling back to defaults");
                async_nats::ConnectOptions::new().name("petri-executor")
            }
        };

        match options
            .connect(&executor_nats_url)
            .await
        {
            Ok(client) => {
                let executor_js = async_nats::jetstream::new(client.clone());
                if let Some(ecfg) =
                    engine_config.build_executor_integration_config(client.clone(), executor_js)
                {
                    registry.set_executor_config(ecfg);
                }
                Some(client)
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to connect to executor NATS, executor disabled");
                None
            }
        }
    } else {
        None
    };

    // Configure data catalogue (NATS request-reply to Mekhan for lookups/subscriptions)
    #[cfg(feature = "catalogue")]
    {
        // Reuse the executor NATS connection if available, otherwise use the main one
        let catalogue_nats_client = {
            #[cfg(feature = "executor")]
            {
                match executor_nats_client.as_ref() {
                    Some(c) => c.clone(),
                    None => config
                        .connect()
                        .await
                        .expect("NATS connection for catalogue client"),
                }
            }
            #[cfg(not(feature = "executor"))]
            {
                config
                    .connect()
                    .await
                    .expect("NATS connection for catalogue client")
            }
        };
        registry.set_catalogue_config(petri_api::CatalogueIntegrationConfig {
            nats_client: catalogue_nats_client,
        });
        info!("Data catalogue integration enabled");
    }

    // Set up per-net callbacks: register spawn_net effect handler + execution config.
    // Note: Bridge, signal, and human result listeners are now global.
    {
        let js_for_spawn = jetstream.clone();
        let exec_config = engine_config.build_execution_config();

        registry.set_on_create(Arc::new(move |instance| {
            let net_id = instance.net_id.clone();

            // Apply execution config (schema validation settings)
            instance.service.set_execution_config(exec_config.clone());

            // Register spawn_net effect handler (uses JetStream to create child nets)
            let spawn_handler = petri_nats::SpawnNetHandler::new(
                js_for_spawn.clone(),
                &net_id,
            );
            instance
                .service
                .register_effect_handler(
                    petri_domain::effects::SPAWN_NET.handler_id,
                    Arc::new(spawn_handler),
                )
                .expect("register spawn_net effect handler");
        }));
    }

    // Start NomadWatcher if Nomad backend is configured
    #[cfg(feature = "nomad")]
    let _nomad_watcher_handle = {
        let nomad_cfg = petri_nomad::NomadConfig::from_env();
        if nomad_cfg.is_some() && engine_config.scheduler_backend.as_deref() == Some("nomad") {
            let nomad_cfg = nomad_cfg.unwrap();
            match petri_nomad::NomadWatcher::new(nomad_cfg.clone(), jetstream.clone()).await {
                Ok(watcher) => {
                    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);
                    info!(addr = %nomad_cfg.addr, "Starting Nomad event watcher");
                    let handle = tokio::spawn(async move {
                        watcher.run(shutdown_rx).await;
                    });
                    Some((handle, shutdown_tx))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to create NomadWatcher");
                    None
                }
            }
        } else {
            None
        }
    };

    // Start SlurmWatcher if Slurm backend is configured
    #[cfg(feature = "slurm")]
    let _slurm_watcher_handle = {
        if let (Some(slurm_cfg), Some("slurm")) = (
            petri_slurm::SlurmConfig::from_env(),
            engine_config.scheduler_backend.as_deref(),
        ) {
            match petri_slurm::SlurmWatcher::new(slurm_cfg.clone(), jetstream.clone()).await {
                Ok(watcher) => {
                    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);
                    info!(
                        host = %slurm_cfg.ssh_host,
                        user = %slurm_cfg.ssh_user,
                        "Starting Slurm poll watcher"
                    );
                    let handle = tokio::spawn(async move {
                        watcher.run(shutdown_rx).await;
                    });
                    Some((handle, shutdown_tx))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to create SlurmWatcher");
                    None
                }
            }
        } else {
            None
        }
    };

    // Create global executor SSE broadcast channel + backfill buffer
    #[cfg(feature = "executor")]
    let executor_sse_tx = Arc::new(tokio::sync::broadcast::channel::<petri_executor::ExecutorSseEvent>(512).0);
    #[cfg(feature = "executor")]
    let executor_sse_buffer: petri_executor::ExecutorSseBuffer =
        Arc::new(std::sync::RwLock::new(Vec::new()));

    // Start ExecutorWatcher if executor is enabled
    #[cfg(feature = "executor")]
    let _executor_watcher_handle = {
        if executor_nats_client.is_some() && engine_config.is_executor_enabled() {
            let executor_config = petri_executor::ExecutorConfig::from_env().unwrap_or_default();
            match petri_executor::ExecutorWatcher::new(
                executor_config.clone(),
                async_nats::jetstream::new(executor_nats_client.as_ref().unwrap().clone()),
            )
            .await
            {
                Ok(watcher) => {
                    let watcher = watcher.with_sse_broadcast(executor_sse_tx.clone(), executor_sse_buffer.clone());
                    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);
                    info!(
                        namespace = %executor_config.namespace,
                        status_stream = %executor_config.status_stream,
                        events_stream = %executor_config.events_stream,
                        "Starting executor event watcher (with SSE broadcast)"
                    );
                    let handle = tokio::spawn(async move {
                        watcher.run(shutdown_rx).await;
                    });
                    Some((handle, shutdown_tx))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to create ExecutorWatcher");
                    None
                }
            }
        } else {
            None
        }
    };

    let registry = Arc::new(registry);

    // Install the subworkflow_cancel adapter — needs Arc<NetRegistry> so it
    // can call `terminate` on its own registry. The Timeout node's body
    // cancellation post-pass emits `subworkflow_cancel` effects which route
    // through this adapter.
    registry.set_subworkflow_cancellor(Arc::new(
        petri_api::net_registry::RegistryCancellor::new(registry.clone()),
    ));

    // Start HibernationMaster (watches activity KV, hibernates idle nets)
    if let Some(ref activity) = activity_tracker {
        let hibernator = RegistryHibernator {
            registry: registry.clone(),
        };
        let master = HibernationMaster::new(activity.clone(), Arc::new(hibernator));
        info!("Starting HibernationMaster");
        tokio::spawn(async move {
            if let Err(e) = master.run().await {
                tracing::error!(error = %e, "HibernationMaster stopped with error");
            }
        });
    }

    // Start GlobalSignalListener (routes signals to nets, wakes from hibernation)
    {
        let resolver = RegistryResolver {
            registry: registry.clone(),
            activity: activity_tracker.clone(),
            metadata_kv: metadata_kv_for_resolver.clone(),
        };
        let global_signal = Arc::new(GlobalSignalListener::new(
            jetstream.clone(),
            Arc::new(resolver),
            activity_tracker.clone(),
        ));
        info!("Starting global signal listener");
        let _handle = global_signal.start();
    }

    // Start GlobalBridgeListener (routes bridge tokens to nets, wakes from hibernation)
    {
        let resolver = RegistryBridgeResolver {
            registry: registry.clone(),
            activity: activity_tracker.clone(),
            metadata_kv: metadata_kv_for_resolver.clone(),
        };
        let global_bridge = Arc::new(GlobalBridgeListener::new(
            jetstream.clone(),
            Arc::new(resolver),
            activity_tracker.clone(),
        ));
        info!("Starting global bridge listener");
        let _handle = global_bridge.start();
    }

    // Start GlobalHumanResultListener (routes human results to nets, wakes from hibernation)
    {
        let resolver = RegistryResolver {
            registry: registry.clone(),
            activity: activity_tracker.clone(),
            metadata_kv: metadata_kv_for_resolver,
        };
        let global_human = Arc::new(GlobalHumanResultListener::new(
            jetstream.clone(),
            Arc::new(resolver),
            activity_tracker.clone(),
        ));
        info!("Starting global human result listener");
        let _handles = global_human.start();
    }

    // Start CreateNetListener (creates nets via NATS command)
    {
        let creator = RegistryNetCreator {
            registry: registry.clone(),
            activity: activity_tracker.clone(),
        };
        let create_net_listener = Arc::new(CreateNetListener::new(
            jetstream.clone(),
            Arc::new(creator),
        ));
        info!("Starting create-net command listener");
        let _handle = create_net_listener.start();
    }

    // Net-scoped router (no default net — all access is via /api/nets/{net_id}/*)
    let mut app = create_router_with_registry(registry.clone());

    // Add net metadata discovery + deletion endpoints (requires metadata KV)
    if let Some(kv) = metadata_kv_for_api {
        let discovery_state = NetDiscoveryState {
            registry: registry.clone(),
            metadata_kv: kv.clone(),
        };
        let metadata_route = axum::Router::new()
            .route("/api/nets/metadata", axum::routing::get(list_nets_metadata))
            .with_state(discovery_state);
        app = app.merge(metadata_route);

        let deletion_state = NetDeletionState {
            registry: registry.clone(),
            metadata_kv: kv,
            activity_tracker: activity_tracker.clone(),
            jetstream: jetstream.clone(),
        };
        let delete_route = axum::Router::new()
            .route(
                "/api/nets/:net_id",
                axum::routing::delete(delete_net_handler),
            )
            .with_state(deletion_state);
        app = app.merge(delete_route);

        info!("Net metadata discovery and deletion endpoints enabled");
    }

    // Add executor events SSE endpoint (with backfill from event buffer)
    #[cfg(feature = "executor")]
    {
        use tower_http::cors::{Any, CorsLayer};

        let executor_sse_state = petri_api::handlers::ExecutorSseState {
            tx: executor_sse_tx.clone(),
            buffer: executor_sse_buffer.clone(),
        };
        let executor_sse_route = axum::Router::new()
            .route(
                "/api/executor/events/stream",
                axum::routing::get(petri_api::handlers::executor_event_stream),
            )
            .with_state(executor_sse_state)
            .layer(
                CorsLayer::new()
                    .allow_origin(Any)
                    .allow_methods(Any)
                    .allow_headers(Any),
            );
        app = app.merge(executor_sse_route);
        info!("Executor events SSE endpoint enabled at /api/executor/events/stream");
    }

    start_server(app, engine_config.port).await;

    // Signal shutdown to all event consumers
    shutdown_token.cancel();

    #[cfg(feature = "nomad")]
    if let Some((handle, shutdown_tx)) = _nomad_watcher_handle {
        let _ = shutdown_tx.send(());
        handle.abort();
    }
    #[cfg(feature = "slurm")]
    if let Some((handle, shutdown_tx)) = _slurm_watcher_handle {
        let _ = shutdown_tx.send(());
        handle.abort();
    }
    #[cfg(feature = "executor")]
    if let Some((handle, shutdown_tx)) = _executor_watcher_handle {
        let _ = shutdown_tx.send(());
        handle.abort();
    }
}

/// Start the HTTP server on the configured port.
async fn start_server(app: axum::Router, port: u16) {
    let bind_addr = format!("0.0.0.0:{}", port);

    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .unwrap_or_else(|_| panic!("Failed to bind to {}", bind_addr));

    axum::serve(listener, app)
        .await
        .expect("Failed to start server");
}

async fn ensure_timer_kv(
    jetstream: &async_nats::jetstream::Context,
) -> Result<(), Box<dyn std::error::Error>> {
    use async_nats::jetstream::kv::Config;
    use petri_nats::TIMER_KV_BUCKET;

    let kv_config = Config {
        bucket: TIMER_KV_BUCKET.to_string(),
        // Default TTL for timers if not otherwise specified.
        // Note: Option A uses bucket-level TTL for the "native" expiry.
        // We set it to a large enough value or a specific value for the lab.
        // For dynamic timers, we might need a different strategy, but for now
        // let's use a 1-day default.
        history: 1,
        ..Default::default()
    };

    match jetstream.create_key_value(kv_config).await {
        Ok(_) => {
            info!(bucket = %TIMER_KV_BUCKET, "Timers KV bucket ready");
        }
        Err(e) => {
            tracing::warn!(error = %e, "Could not create timers KV bucket (it may already exist)");
        }
    }

    Ok(())
}

/// Ensure a lifecycle KV bucket exists.
async fn ensure_lifecycle_kv(
    jetstream: &async_nats::jetstream::Context,
    bucket_name: &str,
) -> Option<async_nats::jetstream::kv::Store> {
    use async_nats::jetstream::kv::Config;

    let kv_config = Config {
        bucket: bucket_name.to_string(),
        history: 1,
        ..Default::default()
    };

    match jetstream.create_key_value(kv_config).await {
        Ok(store) => {
            info!(bucket = %bucket_name, "Lifecycle KV bucket ready");
            Some(store)
        }
        Err(_) => {
            // Bucket may already exist, try to get it
            match jetstream.get_key_value(bucket_name).await {
                Ok(store) => {
                    info!(bucket = %bucket_name, "Lifecycle KV bucket already exists");
                    Some(store)
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        bucket = %bucket_name,
                        "Failed to create or get lifecycle KV bucket"
                    );
                    None
                }
            }
        }
    }
}

// =============================================================================
// Trait bridge implementations (main binary bridges petri-nats traits → petri-api types)
// =============================================================================

/// Implements `NetHibernator` for the `NetRegistry`.
struct RegistryHibernator {
    registry: Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
}

#[async_trait::async_trait]
impl petri_nats::NetHibernator for RegistryHibernator {
    async fn hibernate(&self, net_id: &str) -> Result<(), String> {
        self.registry.hibernate(net_id)
    }
}

/// Wraps a `NetInstance` as a `SignalTarget` for the global signal listener.
struct InstanceSignalTarget {
    service: Arc<petri_application::PetriNetService<
        NatsEventStore<MemoryEventStore>,
        MemoryTopologyStore,
        MarkingProjection,
    >>,
    eval_notify: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl petri_nats::SignalTarget for InstanceSignalTarget {
    async fn inject_signal_with_meta(
        &self,
        place_name: &str,
        color: petri_domain::TokenColor,
        reply_routing: Option<petri_domain::ReplyRouting>,
        signal_key: Option<String>,
        dedup_id: Option<String>,
    ) -> Result<(), petri_nats::SignalInjectError> {
        let place_id = petri_domain::PlaceId(place_name.to_string());
        match self
            .service
            .create_token_with_meta(place_id, color, reply_routing, signal_key, dedup_id)
            .await
        {
            Ok(_) => Ok(()),
            Err(petri_application::ServiceError::EventStore(
                petri_application::EventStoreError::Timeout,
            )) => Err(petri_nats::SignalInjectError::Timeout),
            Err(e) => Err(petri_nats::SignalInjectError::Other(e.to_string())),
        }
    }

    fn notify_eval(&self) {
        self.eval_notify.notify_one();
    }
}

/// Implements `NetResolver` for the `NetRegistry` (resolves and wakes nets).
struct RegistryResolver {
    registry: Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
    activity: Option<Arc<ActivityTracker>>,
    /// Metadata KV for tombstone check: reject signals to completed/cancelled nets.
    metadata_kv: Option<async_nats::jetstream::kv::Store>,
}

#[async_trait::async_trait]
impl petri_nats::NetResolver for RegistryResolver {
    async fn resolve_net(&self, net_id: &str) -> Result<Arc<dyn petri_nats::SignalTarget>, String> {
        // Metadata gate: reject signals to unknown, completed, or cancelled nets
        if let Some(ref kv) = self.metadata_kv {
            match kv.get(net_id).await {
                Ok(Some(entry)) => {
                    if let Ok(meta) = serde_json::from_slice::<petri_nats::NetMetadata>(&entry) {
                        if meta.status == petri_nats::NetStatus::Completed
                            || meta.status == petri_nats::NetStatus::Cancelled
                        {
                            return Err(format!(
                                "Net '{}' is {:?} — cannot accept signals",
                                net_id, meta.status
                            ));
                        }
                    }
                }
                Ok(None) => {
                    return Err(format!(
                        "Net '{}' unknown — no metadata entry found",
                        net_id
                    ));
                }
                Err(e) => {
                    return Err(format!(
                        "Net '{}' metadata lookup failed: {}",
                        net_id, e
                    ));
                }
            }
        }

        let instance = self.registry.get_or_create(net_id);

        // Touch activity tracker on wake
        if let Some(ref activity) = self.activity {
            if let Err(e) = activity.touch(net_id).await {
                tracing::warn!(net_id = %net_id, error = %e, "Failed to touch activity on resolve");
            }
        }

        Ok(Arc::new(InstanceSignalTarget {
            service: instance.service.clone(),
            eval_notify: instance.eval_notify.clone(),
        }))
    }
}

/// Implements `MetadataLookup` for the API registry so control-plane handlers
/// can distinguish hibernated nets (rehydratable), tombstoned nets (refuse),
/// and unknown nets (404) after a cold engine boot.
struct KvMetadataLookup {
    metadata_kv: async_nats::jetstream::kv::Store,
}

#[async_trait::async_trait]
impl petri_api::net_registry::MetadataLookup for KvMetadataLookup {
    async fn lookup(&self, net_id: &str) -> petri_api::net_registry::MetadataStatus {
        use petri_api::net_registry::MetadataStatus;
        match self.metadata_kv.get(net_id).await {
            Ok(Some(entry)) => {
                match serde_json::from_slice::<NetMetadata>(&entry) {
                    Ok(meta) => {
                        if meta.status == NetStatus::Completed
                            || meta.status == NetStatus::Cancelled
                        {
                            MetadataStatus::Tombstoned
                        } else {
                            MetadataStatus::Known
                        }
                    }
                    Err(e) => {
                        // Malformed entry — treat as unknown rather than letting
                        // a stray decode error block a real cold-boot request.
                        tracing::warn!(
                            net_id = %net_id,
                            error = %e,
                            "Failed to decode net metadata entry; treating as unknown"
                        );
                        MetadataStatus::Unknown
                    }
                }
            }
            Ok(None) => MetadataStatus::Unknown,
            Err(e) => {
                tracing::warn!(
                    net_id = %net_id,
                    error = %e,
                    "Metadata lookup failed; treating as unknown"
                );
                MetadataStatus::Unknown
            }
        }
    }
}

/// Implements `BridgeResolver` for the `NetRegistry` (resolves nets for bridge token injection).
struct RegistryBridgeResolver {
    registry: Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
    activity: Option<Arc<ActivityTracker>>,
    metadata_kv: Option<async_nats::jetstream::kv::Store>,
}

#[async_trait::async_trait]
impl petri_nats::BridgeResolver for RegistryBridgeResolver {
    async fn resolve_net(&self, net_id: &str) -> Result<Arc<dyn petri_nats::BridgeTarget>, String> {
        // Metadata gate: reject bridge tokens to unknown, completed, or cancelled nets
        if let Some(ref kv) = self.metadata_kv {
            match kv.get(net_id).await {
                Ok(Some(entry)) => {
                    if let Ok(meta) = serde_json::from_slice::<petri_nats::NetMetadata>(&entry) {
                        if meta.status == petri_nats::NetStatus::Completed
                            || meta.status == petri_nats::NetStatus::Cancelled
                        {
                            return Err(format!(
                                "Net '{}' is {:?} — cannot accept bridge tokens",
                                net_id, meta.status
                            ));
                        }
                    }
                }
                Ok(None) => {
                    return Err(format!(
                        "Net '{}' unknown — no metadata entry found",
                        net_id
                    ));
                }
                Err(e) => {
                    return Err(format!(
                        "Net '{}' metadata lookup failed: {}",
                        net_id, e
                    ));
                }
            }
        }

        let instance = self.registry.get_or_create(net_id);

        // Touch activity tracker on wake
        if let Some(ref activity) = self.activity {
            if let Err(e) = activity.touch(net_id).await {
                tracing::warn!(net_id = %net_id, error = %e, "Failed to touch activity on bridge resolve");
            }
        }

        Ok(Arc::new(InstanceBridgeTarget {
            service: instance.service.clone(),
            eval_notify: instance.eval_notify.clone(),
        }))
    }
}

/// Bridge target implementation backed by a NetInstance.
struct InstanceBridgeTarget {
    service: Arc<petri_application::PetriNetService<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
    eval_notify: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl petri_nats::BridgeTarget for InstanceBridgeTarget {
    async fn inject_bridge_token(
        &self,
        place_name: &str,
        color: petri_domain::TokenColor,
        reply_routing: Option<petri_domain::ReplyRouting>,
        signal_key: Option<String>,
        dedup_id: Option<String>,
    ) -> Result<(), petri_nats::BridgeInjectError> {
        let place_id = petri_domain::PlaceId(place_name.to_string());
        match self
            .service
            .create_token_with_meta(place_id, color, reply_routing, signal_key, dedup_id)
            .await
        {
            Ok(_) => Ok(()),
            Err(petri_application::ServiceError::EventStore(
                petri_application::EventStoreError::Timeout,
            )) => Err(petri_nats::BridgeInjectError::Timeout),
            Err(e) => Err(petri_nats::BridgeInjectError::Other(e.to_string())),
        }
    }

    fn notify_eval(&self) {
        self.eval_notify.notify_one();
    }
}

/// Implements `NetCreator` for the `NetRegistry` (creates nets via NATS command).
struct RegistryNetCreator {
    registry: Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
    activity: Option<Arc<ActivityTracker>>,
}

#[async_trait::async_trait]
impl petri_nats::NetCreator for RegistryNetCreator {
    async fn create_and_load(
        &self,
        request: &petri_nats::CreateNetRequest,
    ) -> Result<(), String> {
        let instance = self.registry.get_or_create(&request.net_id);

        // Emit NetCreated lifecycle event
        let event = petri_domain::DomainEvent::NetCreated {
            net_id: request.net_id.clone(),
            template_id: request.template_id.clone(),
            parameters: request.parameters.clone(),
            created_by: request.created_by.clone(),
            label: request.label.clone(),
        };
        instance
            .service
            .append_event(event)
            .await
            .map_err(|e| e.to_string())?;


        // Store net parameters on the service for $params. resolution in bridge targets
        if let Some(ref params) = request.parameters {
            instance.service.set_net_parameters(params.clone());
        }

        // Parse and load the scenario
        // The scenario JSON is expected to be a LoadScenarioRequest-compatible format
        let scenario: petri_api::dto::LoadScenarioRequest =
            serde_json::from_value(request.scenario.clone())
                .map_err(|e| format!("Invalid scenario JSON: {}", e))?;

        let parsed = petri_api::ScenarioBridge::parse(
            &scenario.places,
            &scenario.transitions,
            scenario.definitions.clone(),
        )
        .map_err(|e| format!("Failed to parse scenario: {}", e))?;

        let initial_tokens = parsed.initial_tokens.clone();

        // Clear and initialize
        instance.service.clear().await;
        instance
            .service
            .initialize(parsed.net)
            .await
            .map_err(|e| e.to_string())?;
        instance.service.set_initial_tokens(initial_tokens.clone());

        // Load schema registry if definitions present
        if !parsed.definitions.is_empty() {
            match petri_application::SchemaRegistry::new(parsed.definitions) {
                Ok(registry) => {
                    instance.service.set_schema_registry(registry);
                }
                Err(e) => {
                    return Err(format!("Failed to compile schema definitions: {}", e));
                }
            }
        }

        // Create initial tokens (from scenario definition)
        for (place_id, color) in initial_tokens {
            instance
                .service
                .create_token(place_id, color)
                .await
                .map_err(|e| e.to_string())?;
        }

        // Inject initial tokens from the request (e.g., from spawn effect)
        // Uses create_token_with_meta to propagate bridge metadata for correlation.
        if let Some(ref tokens) = request.initial_tokens {
            for it in tokens {
                let place_id = petri_domain::PlaceId(it.place_id.clone());
                let color = petri_application::json_to_token_color(&it.token);
                instance
                    .service
                    .create_token_with_meta(place_id, color, it.reply_routing.clone(), None, None)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }

        // Switch to Running so the eval loop picks up the injected tokens.
        // Spawned nets should run immediately — the parent was necessarily
        // running when it fired the spawn effect.
        *instance.run_mode.write() = petri_api::dto::RunMode::Running;
        instance.eval_notify.notify_one();

        // Touch activity tracker
        if let Some(ref activity) = self.activity {
            if let Err(e) = activity.touch(&request.net_id).await {
                tracing::warn!(
                    net_id = %request.net_id,
                    error = %e,
                    "Failed to touch activity after net creation"
                );
            }
        }

        Ok(())
    }
}

/// Ensure the NATS JetStream streams exist.
async fn ensure_streams(
    jetstream: &async_nats::jetstream::Context,
) -> Result<(), Box<dyn std::error::Error>> {
    use async_nats::jetstream::stream::{Config as StreamConfig, RetentionPolicy};
    use std::time::Duration;

    // Use the shared stream config from petri-nats to ensure consistency
    // across the engine, listeners, and bridges.
    match jetstream
        .get_or_create_stream(petri_nats::stream_config())
        .await
    {
        Ok(stream) => {
            let stream_name = stream.cached_info().config.name.clone();
            info!(
                name = %stream_name,
                "Global stream ready (single stream architecture)"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "Could not create global stream");
        }
    }

    // Create human task streams (cancel, cancelled, failed)
    // HUMAN_REQUESTS and HUMAN_COMPLETED are created by the human client and UI respectively.
    {
        let human_streams = [
            (Subjects::STREAM_HUMAN_CANCEL, Subjects::HUMAN_CANCEL_PREFIX),
            (Subjects::STREAM_HUMAN_CANCELLED, Subjects::HUMAN_CANCELLED_PREFIX),
            (Subjects::STREAM_HUMAN_FAILED, Subjects::HUMAN_FAILED_PREFIX),
        ];
        for (stream_name, prefix) in human_streams {
            match jetstream
                .get_or_create_stream(StreamConfig {
                    name: stream_name.to_string(),
                    subjects: vec![format!("{}.>", prefix)],
                    retention: RetentionPolicy::Limits,
                    max_age: Duration::from_secs(7 * 24 * 60 * 60),
                    ..Default::default()
                })
                .await
            {
                Ok(_) => info!(name = %stream_name, "Human stream ready"),
                Err(e) => tracing::warn!(error = %e, name = %stream_name, "Could not create human stream"),
            }
        }
    }

    Ok(())
}

// =============================================================================
// Net metadata discovery endpoint
// =============================================================================

/// Combined state for the `/api/nets/metadata` endpoint.
#[derive(Clone)]
struct NetDiscoveryState {
    registry: Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
    metadata_kv: async_nats::jetstream::kv::Store,
}

/// Response type for net metadata discovery.
#[derive(serde::Serialize)]
struct NetMetadataInfo {
    net_id: String,
    status: NetStatus,
    /// Whether the net is currently loaded in memory (hot) or hibernated.
    in_memory: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    template_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

/// GET /api/nets/metadata — list all nets with metadata (including hibernated).
///
/// Cross-references the `KV_NET_METADATA` bucket with the in-memory registry
/// to determine which nets are "hot" (in-memory) vs "hibernated".
async fn list_nets_metadata(
    axum::extract::State(state): axum::extract::State<NetDiscoveryState>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use futures::StreamExt;

    let in_memory_nets: std::collections::HashSet<String> =
        state.registry.list().into_iter().collect();

    let keys = match state.metadata_kv.keys().await {
        Ok(k) => k,
        Err(e) => {
            tracing::error!(error = %e, "Failed to list metadata keys");
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": "Failed to list net metadata"})),
            )
                .into_response();
        }
    };

    tokio::pin!(keys);
    let mut results = Vec::new();

    while let Some(key) = keys.next().await {
        let net_id = match key {
            Ok(k) => k,
            Err(_) => continue,
        };

        if let Ok(Some(entry)) = state.metadata_kv.get(&net_id).await {
            if let Ok(meta) = serde_json::from_slice::<NetMetadata>(&entry) {
                results.push(NetMetadataInfo {
                    in_memory: in_memory_nets.contains(&meta.net_id),
                    net_id: meta.net_id,
                    status: meta.status,
                    template_id: meta.template_id,
                    parameters: meta.parameters,
                    created_by: meta.created_by,
                    label: meta.label,
                });
            }
        }
    }

    axum::Json(results).into_response()
}

// =============================================================================
// Net deletion endpoint (full lifecycle cleanup)
// =============================================================================

/// Combined state for the `DELETE /api/nets/{net_id}` handler.
///
/// Needs access to the registry (hot nets), metadata KV (hibernated nets),
/// activity tracker, and JetStream (direct event publishing for hibernated nets).
#[derive(Clone)]
struct NetDeletionState {
    registry: Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
    metadata_kv: async_nats::jetstream::kv::Store,
    activity_tracker: Option<Arc<ActivityTracker>>,
    jetstream: async_nats::jetstream::Context,
}

/// DELETE /api/nets/{net_id} — properly terminate and clean up a net.
///
/// Handles three cases:
/// 1. Hot net (in registry): terminate with lifecycle event + cancel tasks.
/// 2. Hibernated net (in KV but not registry): publish NetCancelled directly to NATS.
/// 3. Not found: return 404.
async fn delete_net_handler(
    axum::extract::State(state): axum::extract::State<NetDeletionState>,
    axum::extract::Path(net_id): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    // Case 1: Hot net — terminate properly (emits NetCancelled, cancels tasks)
    if state.registry.get(&net_id).is_some() {
        match state
            .registry
            .terminate(
                &net_id,
                Some("Deleted by user".to_string()),
                Some("engine-api".to_string()),
            )
            .await
        {
            Ok(()) => {
                if let Some(ref activity) = state.activity_tracker {
                    let _ = activity.remove(&net_id).await;
                }
                tracing::info!(net_id = %net_id, "Net deleted (was hot)");
                return StatusCode::NO_CONTENT.into_response();
            }
            Err(e) => {
                tracing::error!(net_id = %net_id, error = %e, "Failed to terminate hot net");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({"error": format!("Failed to terminate: {}", e)})),
                )
                    .into_response();
            }
        }
    }

    // Case 2 & 3: Check metadata KV for hibernated or completed nets
    let meta = match state.metadata_kv.get(&net_id).await {
        Ok(Some(entry)) => serde_json::from_slice::<NetMetadata>(&entry).ok(),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(net_id = %net_id, error = %e, "Failed to read metadata KV");
            None
        }
    };

    match meta {
        Some(meta) if meta.status == NetStatus::Running || meta.status == NetStatus::Created => {
            // Case 2: Hibernated net — publish NetCancelled directly to NATS
            let event = petri_domain::DomainEvent::NetCancelled {
                net_id: net_id.clone(),
                reason: Some("Deleted by user".to_string()),
                cancelled_by: Some("engine-api".to_string()),
            };
            let persisted = petri_domain::PersistedEvent::new(0, event, None);
            let subject = Subjects::for_event(&persisted.event, Some(&net_id));
            let payload = match serde_json::to_vec(&persisted) {
                Ok(p) => p,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        axum::Json(
                            serde_json::json!({"error": format!("Serialization failed: {}", e)}),
                        ),
                    )
                        .into_response();
                }
            };

            match state.jetstream.publish(subject, payload.into()).await {
                Ok(ack_future) => {
                    if let Err(e) = ack_future.await {
                        tracing::warn!(net_id = %net_id, error = %e, "NATS ACK failed for NetCancelled");
                    }
                }
                Err(e) => {
                    tracing::error!(net_id = %net_id, error = %e, "Failed to publish NetCancelled");
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        axum::Json(
                            serde_json::json!({"error": format!("NATS publish failed: {}", e)}),
                        ),
                    )
                        .into_response();
                }
            }

            if let Some(ref activity) = state.activity_tracker {
                let _ = activity.remove(&net_id).await;
            }
            tracing::info!(net_id = %net_id, "Net deleted (was hibernated)");
            StatusCode::NO_CONTENT.into_response()
        }
        Some(_) => {
            // Already completed/cancelled — clean up any leftover activity entry
            if let Some(ref activity) = state.activity_tracker {
                let _ = activity.remove(&net_id).await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        None => {
            (
                StatusCode::NOT_FOUND,
                axum::Json(serde_json::json!({"error": "Net not found"})),
            )
                .into_response()
        }
    }
}
