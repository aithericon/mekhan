mod config;
#[allow(dead_code)]
mod hydration;

/// Heap profiling (build with `--features dhat-heap`). dhat measures LOGICAL live
/// allocations — what the program holds, independent of the allocator/libc — so it
/// distinguishes *live data* from *allocator slack*. On graceful shutdown (SIGINT/
/// SIGTERM → `start_server` returns → `main` returns) the held `Profiler` drops and
/// writes `dhat-heap.json`. Off by default (system allocator, zero overhead).
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use std::sync::Arc;

use petri_api::HumanIntegrationConfig;
use petri_api::{create_router_with_registry, NetRegistry};
use petri_infrastructure::{MarkingProjection, MemoryEventStore, MemoryTopologyStore};
use petri_nats::human_client::HumanNatsClient;
use petri_nats::GlobalHumanResultListener;
use petri_nats::Subjects;
use petri_nats::{
    ActivityTracker, Clockmaster, CreateNetListener, EventConsumer, GlobalBridgeListener,
    GlobalSignalListener, HibernationMaster, NatsConfig, NatsEventStore, NatsTimerClient,
    NetMetadataProjection, ACTIVITY_KV_BUCKET, METADATA_KV_BUCKET,
};
use petri_nats::{NetMetadata, NetStatus};
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::EngineConfig;

#[tokio::main]
async fn main() {
    // Heap profiler guard (feature `dhat-heap`). Held for all of `main`; on
    // graceful shutdown it drops and writes dhat-heap.json.
    #[cfg(feature = "dhat-heap")]
    let _dhat_profiler = dhat::Profiler::new_heap();

    // Initialize tracing (override with RUST_LOG env var)
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,petri_application=debug,petri_api=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Tracing initialized");

    // Dev-only Nomad parameterized-job self-heal (gated by NOMAD_AUTOPROVISION_JOBS=1;
    // never set in prod). The in-memory dev nomad agent loses its jobs on restart, so
    // re-register them at every engine boot before any lease/scheduler dispatch.
    #[cfg(feature = "nomad")]
    petri_api::nomad_allocator::ensure_parameterized_jobs().await;

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

    // Ensure the timers KV bucket exists. The live bucket is per-workspace
    // (`KV_TIMERS_{ws}`); pre-create the process-workspace one here so the
    // single shared `NatsTimerClient` / `Clockmaster` (which `get` the bucket,
    // not create it) find it. Per-tenant timer buckets for non-default
    // workspaces are pre-created on demand below.
    // TODO(stream-per-ws): create one `KV_TIMERS_{ws}` per workspace + a
    // per-ws Clockmaster lifecycle.
    let process_workspace = config.workspace().to_string();
    if let Err(e) = ensure_timer_kv(&jetstream, &process_workspace).await {
        tracing::warn!(error = %e, "Failed to create timers KV bucket");
    }

    // Initialize timer client and clockmaster.
    //
    // Single process-workspace `NatsTimerClient` + `Clockmaster` watching
    // `KV_TIMERS_{process_ws}`. Tenant isolation for the FIRE path is preserved
    // even with one shared watcher: each scheduled `TimerValue` carries its
    // net's real `workspace_id` (the timer handler shares the service's
    // workspace cell), and the Clockmaster fires `petri.{timer.ws}.{net}.
    // signal.{place}` under THAT workspace, not its own.
    // TODO(stream-per-ws): one `Clockmaster` per workspace watching
    // `KV_TIMERS_{ws}` (true per-tenant timer buckets); the `TimerValue.
    // workspace_id` carry becomes redundant then.
    let timer_client = match NatsTimerClient::new(&jetstream, &process_workspace).await {
        Ok(client) => Some(Arc::new(client)),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to create NatsTimerClient, timers disabled");
            None
        }
    };

    if let Ok(clockmaster) = Clockmaster::new(jetstream.clone(), &process_workspace).await {
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
    // Single process-level activity tracker. This stays correct under
    // multi-tenancy WITHOUT a per-ws bucket because the tracker is keyed by
    // net_id, which is a globally-unique UUID (mekhan derives it from
    // `uuid::Uuid`) — two tenants can never collide on a net_id, so one shared
    // activity bucket isolates hibernation correctly.
    // TODO(stream-per-ws): per-workspace activity buckets (ws parsed from the
    // listener subject) if hibernation policy ever needs to differ per tenant.
    let activity_tracker = activity_kv.map(|kv| {
        Arc::new(ActivityTracker::new(
            kv,
            idle_timeout,
            process_workspace.clone(),
        ))
    });

    // Clone metadata KV for the resolver (tombstone check on signal routing)
    let metadata_kv_for_resolver = metadata_kv.as_ref().cloned();

    // Clone metadata KV for the API discovery endpoint (before projection consumes it)
    let metadata_kv_for_api = metadata_kv.as_ref().cloned();

    // Clone metadata KV for the registry so control-plane handlers can rehydrate
    // hibernated nets after a cold engine boot.
    let metadata_kv_for_registry = metadata_kv.as_ref().cloned();

    // Start net metadata projection.
    //
    // ONE global consumer over `petri.*.events.>`: it derives the workspace
    // per-event from the subject and DUAL-WRITES the global net_id-keyed index
    // bucket (`KV_NET_METADATA`, the index every net_id-only reader uses) plus
    // the per-tenant `KV_NET_METADATA_{ws}` bucket (isolation). The index entry
    // now carries `workspace_id`, so the woken-net resolver can recover a net's
    // tenant from it (hazard #2).
    // TODO(stream-per-ws): replace with one `net-metadata-projection-{ws}`
    // durable per workspace once the stream is sharded per tenant.
    if let Some(kv) = metadata_kv {
        let projection = NetMetadataProjection::new(jetstream.clone(), kv);
        info!("Starting net metadata projection (global, ws-deriving)");
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

        // Create the consumer (handles hydration + live consumption). It is NOT
        // started here (multi-tenancy linchpin): at factory time the net's real
        // workspace is unknown, so an eager start would filter
        // `petri.{process_fallback}.{net}.events.>` — the wrong tenant. We hand
        // the consumer to a `ConsumerStarter` the registry invokes AFTER the
        // workspace is stamped (per-net `load_scenario` / `create_and_load`),
        // filtering on the real workspace.
        // Snapshot plumbing (PART C): a shared cell the consumer publishes its
        // last-applied JetStream `stream_sequence` into (read by hibernate for
        // the snapshot's `last_stream_seq`), and a resume-from cell the wake
        // path sets so the consumer hydrates only the post-snapshot delta.
        let last_stream_seq_cell = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let resume_from_cell: Arc<parking_lot::RwLock<Option<u64>>> =
            Arc::new(parking_lot::RwLock::new(None));

        let consumer =
            EventConsumer::new(cache.clone(), topology_store.clone(), applied_tx, ready_tx)
                .with_last_stream_seq_cell(last_stream_seq_cell.clone());

        // Shared per-net workspace cell: the publisher reads it, and
        // `PetriNetService::set_workspace_id` (which the registry builds against
        // this same Arc) writes through it. The consumer-starter also reads the
        // ws passed to it (resolved from the service post-stamp).
        let workspace_cell: Arc<std::sync::RwLock<Option<String>>> =
            Arc::new(std::sync::RwLock::new(None));

        // Each net gets its own config with the correct net_id for bridge routing.
        let mut net_cfg = cfg.clone();
        net_cfg.net_id = Some(net_id.to_string());
        let applied_rx_for_registry = applied_rx.clone();
        let event_store = Arc::new(NatsEventStore::new_with_workspace(
            cache,
            js.clone(),
            net_cfg,
            applied_rx,
            workspace_cell.clone(),
        ));

        // Build the deferred consumer starter. Invoked once by the registry
        // under the real per-net workspace: it spawns the consumer (filtered on
        // `petri.{ws}.{net}.events.>`) and blocks on hydration so a woken net's
        // history is replayed before the eval loop consults topology. The
        // `ready_rx` is wrapped in a Mutex<Option<>> so the `Fn` closure can
        // `take` it on the single invocation (FnOnce semantics inside an Fn).
        let js_consumer = js.clone();
        let net_id_consumer = net_id.to_string();
        let shutdown = shutdown_for_factory.clone();
        let consumer_cell = std::sync::Arc::new(std::sync::Mutex::new(Some((consumer, ready_rx))));
        let resume_cell_for_starter = resume_from_cell.clone();
        let consumer_starter: petri_api::net_registry::ConsumerStarter =
            Arc::new(move |ws: String, net_cancel: tokio_util::sync::CancellationToken| {
                let js_consumer = js_consumer.clone();
                let net_id_consumer = net_id_consumer.clone();
                let shutdown = shutdown.clone();
                let consumer_cell = consumer_cell.clone();
                let resume_cell = resume_cell_for_starter.clone();
                Box::pin(async move {
                    let Some((mut consumer, ready_rx)) = consumer_cell.lock().unwrap().take()
                    else {
                        // Already started — idempotent no-op.
                        return;
                    };
                    // Snapshot wake (PART C): if the registry seeded the store and
                    // set a resume point, hydrate only the post-snapshot delta.
                    if let Some(resume_from) = *resume_cell.read() {
                        consumer = consumer.with_resume_from(resume_from);
                    }
                    // The consumer must stop when EITHER the process shuts down OR
                    // this net hibernates/completes. Binding it only to the global
                    // token leaked the net's `Arc<MemoryEventStore>` forever (the
                    // orphaned consumer outlived the net). Derive a token cancelled
                    // by either source; the linker task exits as soon as one fires.
                    let consumer_shutdown = tokio_util::sync::CancellationToken::new();
                    {
                        let cs = consumer_shutdown.clone();
                        let global = shutdown.clone();
                        let net = net_cancel.clone();
                        tokio::spawn(async move {
                            tokio::select! {
                                _ = global.cancelled() => {}
                                _ = net.cancelled() => {}
                            }
                            cs.cancel();
                        });
                    }
                    tokio::spawn(async move {
                        if let Err(e) = consumer
                            .start(&js_consumer, &ws, &net_id_consumer, consumer_shutdown)
                            .await
                        {
                            tracing::error!(
                                error = %e,
                                net_id = %net_id_consumer,
                                workspace = %ws,
                                "Event consumer stopped with error"
                            );
                        }
                    });
                    // Wait for hydration to complete before returning. For a
                    // fresh net this resolves immediately (no history); for a
                    // woken net it blocks until replay finishes so the registry
                    // sees populated topology.
                    if ready_rx.await.is_err() {
                        tracing::warn!(
                            "Event consumer ready signal dropped (consumer may have failed)"
                        );
                    }
                })
            });

        (
            event_store,
            topology_store,
            Arc::new(MarkingProjection::new()),
            applied_rx_for_registry,
            workspace_cell,
            consumer_starter,
            last_stream_seq_cell,
            resume_from_cell,
        )
    });

    let mut registry = NetRegistry::new(store_factory);
    if let Some(kv) = metadata_kv_for_registry {
        registry.set_metadata_lookup(Arc::new(KvMetadataLookup {
            metadata_kv: kv.clone(),
        }));
        // Woken-net workspace resolver (multi-tenancy linchpin / hazard #2):
        // when a hibernated net is rehydrated, `get_or_create` consults this to
        // stamp + start its event consumer under the REAL workspace before
        // consulting topology. The projection persists each net's workspace on
        // the global `KV_NET_METADATA` index (derived from the event subject);
        // this resolver returns it VERBATIM for any existing entry — including
        // the default workspace — so a woken net always hydrates from
        // `petri.{realws}.{net}.events.>` and its consumer is actually started.
        // `None` is reserved for "no entry" (a genuinely fresh net), whose
        // consumer start is DEFERRED to `load_scenario`. See
        // `KvWokenWorkspaceResolver` for why collapsing the default workspace
        // into `None` silently broke hibernated capacity-pool bridges.
        registry.set_woken_workspace_resolver(Arc::new(KvWokenWorkspaceResolver {
            metadata_kv: kv,
        }));
    }
    // Let the HTTP command handlers record net activity, so an HTTP-driven net
    // has the same idle/hibernation lifecycle as a NATS-stimulated one (the
    // NATS listeners already touch this same tracker). No-op if hibernation is
    // disabled (no activity KV).
    if let Some(ref activity) = activity_tracker {
        registry.set_activity_sink(activity.clone());
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

    // Sub-phase 2.3b: detect cloud-layer HTTP dispatch mode. When
    // `MEKHAN_EXECUTOR_DISPATCH=http`, register the HTTP-based
    // `HttpInferenceHandler` against `executor_submit` and skip the
    // NATS-executor connection entirely. Default (unset / `nats`) keeps the
    // existing NATS-dispatch path. The two modes are mutually exclusive at
    // the registry level (`get_or_create` asserts).
    #[cfg(feature = "executor")]
    let use_http_dispatch = std::env::var("MEKHAN_EXECUTOR_DISPATCH")
        .ok()
        .map(|v| v.eq_ignore_ascii_case("http"))
        .unwrap_or(false);

    #[cfg(feature = "executor")]
    if use_http_dispatch {
        registry.set_http_executor_config(petri_api::HttpExecutorConfig::default());
        info!(
            "Executor dispatch mode: HTTP (cloud-layer; sub-phase 2.3b) — NATS executor connection skipped"
        );
    }

    // Connect executor NATS client and set config on registry (behind feature gate)
    #[cfg(feature = "executor")]
    let executor_nats_client = if use_http_dispatch {
        // HTTP-dispatch mode short-circuits the NATS-executor connection.
        None
    } else if engine_config.is_executor_enabled() {
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

        match options.connect(&executor_nats_url).await {
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

            // Register spawn_net effect handler (uses JetStream to create child nets).
            // Spawns are intra-workspace: stamp the parent's workspace so spawned
            // children are created under `petri.{ws}.commands.create_net`.
            let parent_workspace = instance
                .service
                .workspace()
                .unwrap_or_else(|| Subjects::DEFAULT_WORKSPACE.to_string());
            let spawn_handler = petri_nats::SpawnNetHandler::new(js_for_spawn.clone(), &net_id)
                .with_workspace(parent_workspace);
            instance
                .service
                .register_effect_handler(
                    petri_domain::effects::SPAWN_NET.handler_id,
                    Arc::new(spawn_handler),
                )
                .expect("register spawn_net effect handler");
        }));
    }

    // Multi-cluster scheduling (docs/16): the engine no longer starts ONE
    // boot-time Slurm/Nomad watcher from env. Instead it constructs a
    // `ClusterRegistry` that lazily builds a per-`(resource_id, version)`
    // `ClusterClient` (allocator + per-cluster watcher) on the first lease/submit
    // that references it, from the connection riding the effect_config, and
    // idle-tears-down a cluster when no leases reference it. The registry is
    // installed on the `NetRegistry` below (before the first `get_or_create`) and
    // held here for the `GET /api/clusters` management surface.
    //
    // The single dev-bootstrap env path (`SLURM_SSH_HOST`/`NOMAD_ADDR` set, no
    // datacenter resource) is preserved by `NetRegistry::build_env_flavor_dispatch`
    // — when no registry is installed the legacy env dispatcher serves, so
    // `just dev slurm-up`/`scheduler-up` keep working. (A future phase can also
    // pre-build a dev-bootstrap `ClusterClient` under the reserved `_env` key.)
    #[cfg(any(feature = "slurm", feature = "nomad"))]
    let cluster_registry = std::sync::Arc::new(petri_api::cluster_registry::ClusterRegistry::new(
        jetstream.clone(),
    ));

    // Create global executor SSE broadcast channel + backfill buffer
    #[cfg(feature = "executor")]
    let executor_sse_tx =
        Arc::new(tokio::sync::broadcast::channel::<petri_executor::ExecutorSseEvent>(512).0);
    #[cfg(feature = "executor")]
    let executor_sse_buffer: petri_executor::ExecutorSseBuffer =
        Arc::new(std::sync::RwLock::new(Vec::new()));

    // Start ExecutorWatcher if executor is enabled
    #[cfg(feature = "executor")]
    let _executor_watcher_handle = {
        if let (Some(client), true) = (
            executor_nats_client.as_ref(),
            engine_config.is_executor_enabled(),
        ) {
            let executor_config = petri_executor::ExecutorConfig::from_env().unwrap_or_default();
            match petri_executor::ExecutorWatcher::new(
                executor_config.clone(),
                async_nats::jetstream::new(client.clone()),
            )
            .await
            {
                Ok(watcher) => {
                    let watcher = watcher
                        .with_sse_broadcast(executor_sse_tx.clone(), executor_sse_buffer.clone());
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

    // Install the wake-snapshot store (PART C). Backed by an OpenDAL object
    // store (S3/GCS/Azure/local fs), configured via `PETRI_SNAPSHOT_STORE_*`.
    // With it installed, hibernate captures a snapshot and the wake path replays
    // only the post-snapshot delta — a cold wake of a huge net is then O(events
    // since hibernate), not O(total events). Every failure mode degrades to full
    // replay, so this is a pure fast-path.
    match petri_api::ObjectSnapshotStore::from_env() {
        Some(store) => {
            info!("Snapshot store: object store (OpenDAL)");
            registry.set_snapshot_store(Arc::new(store));
        }
        None => {
            tracing::warn!(
                "Snapshot store: DISABLED (PETRI_SNAPSHOT_STORE_* unset) — wakes will full-replay"
            );
        }
    }

    // Install the subworkflow_cancel adapter — needs Arc<NetRegistry> so it
    // can call `terminate` on its own registry. The Timeout node's body
    // cancellation post-pass emits `subworkflow_cancel` effects which route
    // through this adapter.
    registry.set_subworkflow_cancellor(Arc::new(petri_api::net_registry::RegistryCancellor::new(
        registry.clone(),
    )));

    // Install the multi-cluster ClusterRegistry BEFORE the first get_or_create
    // so every net's resource_lease handlers route through it (docs/16). Must
    // precede any net instantiation (the pre-dispatch freeze + handler wiring).
    #[cfg(any(feature = "slurm", feature = "nomad"))]
    registry.set_cluster_registry(cluster_registry.clone());

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
        let create_net_listener =
            Arc::new(CreateNetListener::new(jetstream.clone(), Arc::new(creator)));
        info!("Starting create-net command listener");
        let _handle = create_net_listener.start();
    }

    // Net-scoped router (no default net — all access is via /api/nets/{net_id}/*)
    let mut app = create_router_with_registry(registry.clone());

    // First-class cluster/watcher management (docs/16 §9): GET /api/clusters +
    // force-reconnect/drain over the live `ClusterRegistry`. mekhan reads this
    // through as /api/v1/clusters.
    #[cfg(any(feature = "slurm", feature = "nomad"))]
    {
        app = app.merge(petri_api::cluster_routes::cluster_routes(
            cluster_registry.clone(),
        ));
    }

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

    // Raise the request-body limit well above axum's 2 MiB default: a compiled
    // net for a workflow that inlines several SubWorkflows (e.g. the xArm
    // sample-handling capstone, which inlines pick + place + swap, each now with
    // approach + cartesian-descent legs) serializes past 2 MiB and the deploy
    // POST 413s. 32 MiB leaves ample headroom (cf. the 8 MiB NATS max_payload).
    let app = app.layer(axum::extract::DefaultBodyLimit::max(32 * 1024 * 1024));

    start_server(app, engine_config.port).await;

    // Signal shutdown to all event consumers
    shutdown_token.cancel();

    // Per-cluster Slurm/Nomad watchers are owned by the `ClusterRegistry` now
    // (lazy + idle-torn-down); they stop on idle-teardown / cancel rather than a
    // boot-time shutdown handle (docs/16).
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
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("Failed to start server");
}

/// Resolves on SIGINT (Ctrl-C) or SIGTERM so the server returns cleanly, letting
/// `main` run its shutdown path — and, under `--features dhat-heap`, letting the
/// held `dhat::Profiler` drop and write its report.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("Shutdown signal received — stopping server");
}

async fn ensure_timer_kv(
    jetstream: &async_nats::jetstream::Context,
    workspace: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use async_nats::jetstream::kv::Config;
    use petri_nats::TIMER_KV_BUCKET;

    // Per-workspace bucket: `KV_TIMERS_{ws}`. The timer client + Clockmaster
    // open this by name (they don't create it), so it must exist first.
    let bucket = petri_nats::kv_bucket_for(TIMER_KV_BUCKET, workspace);

    let kv_config = Config {
        bucket: bucket.clone(),
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
            info!(bucket = %bucket, "Timers KV bucket ready");
        }
        Err(e) => {
            tracing::warn!(error = %e, bucket = %bucket, "Could not create timers KV bucket (it may already exist)");
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
    registry:
        Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
}

#[async_trait::async_trait]
impl petri_nats::NetHibernator for RegistryHibernator {
    async fn hibernate(&self, net_id: &str) -> Result<(), String> {
        self.registry.hibernate(net_id).await
    }
}

/// Wraps a `NetInstance` as a `SignalTarget` for the global signal listener.
struct InstanceSignalTarget {
    service: Arc<
        petri_application::PetriNetService<
            NatsEventStore<MemoryEventStore>,
            MemoryTopologyStore,
            MarkingProjection,
        >,
    >,
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
    registry:
        Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
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
                    return Err(format!("Net '{}' metadata lookup failed: {}", net_id, e));
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

/// Resolves a WOKEN net's persisted workspace so `get_or_create` can start its
/// event consumer under the real tenant before consulting topology (hazard #2).
///
/// Reads the net's `workspace_id` off the global net_id-keyed metadata index
/// (`KV_NET_METADATA`), which the projection stamps from the event subject. A
/// woken multi-tenant net therefore hydrates from `petri.{realws}.{net}.
/// events.>` rather than `petri.default.*`.
///
/// `None` means "no metadata entry exists" — a genuinely fresh net whose
/// consumer start the registry DEFERS to `load_scenario` (which stamps the
/// workspace first). `None` does NOT mean "recorded under the default
/// workspace": a net the projection stamped `default` MUST resolve to
/// `Some("default")`, because the woken-net branch of `get_or_create` only
/// starts (and blocks on) the event consumer when this returns `Some`. Folding
/// the default workspace into `None` (the original single-workspace-dev
/// shortcut) left every `default`-recorded net to wake with its consumer never
/// started — so topology never hydrated, `resolve_topology` returned `None`, and
/// a hibernated capacity-pool net (`pool-<resource_id>`) bridged-to by a
/// workflow instance failed the activation gate with `BRIDGE_TARGET_NET_MISSING`
/// until manually re-deployed. Return the recorded workspace verbatim for ANY
/// existing entry.
struct KvWokenWorkspaceResolver {
    metadata_kv: async_nats::jetstream::kv::Store,
}

/// Pure decision behind [`KvWokenWorkspaceResolver::workspace_for`]: given the
/// raw metadata blob (or its absence), return the workspace to wake under.
///
/// `Some(ws)` for any parseable entry (INCLUDING the default workspace); `None`
/// only when there is no entry to read or it fails to parse. Extracted so the
/// "default must not collapse to None" invariant is unit-testable without NATS.
fn woken_workspace_from_metadata(entry: Option<&[u8]>) -> Option<String> {
    let bytes = entry?;
    serde_json::from_slice::<NetMetadata>(bytes)
        .ok()
        .map(|meta| meta.workspace_id)
}

#[async_trait::async_trait]
impl petri_api::net_registry::WokenWorkspaceResolver for KvWokenWorkspaceResolver {
    async fn workspace_for(&self, net_id: &str) -> Option<String> {
        let entry = self.metadata_kv.get(net_id).await.ok().flatten();
        woken_workspace_from_metadata(entry.as_deref())
    }
}

/// Implements `BridgeResolver` for the `NetRegistry` (resolves nets for bridge token injection).
struct RegistryBridgeResolver {
    registry:
        Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
    activity: Option<Arc<ActivityTracker>>,
    metadata_kv: Option<async_nats::jetstream::kv::Store>,
}

#[async_trait::async_trait]
impl petri_nats::BridgeResolver for RegistryBridgeResolver {
    async fn resolve_net(
        &self,
        net_id: &str,
    ) -> Result<Arc<dyn petri_nats::BridgeTarget>, petri_nats::BridgeResolveError> {
        use petri_nats::BridgeResolveError;
        // Metadata gate: reject bridge tokens to unknown, completed, or cancelled nets.
        if let Some(ref kv) = self.metadata_kv {
            match kv.get(net_id).await {
                Ok(Some(entry)) => {
                    if let Ok(meta) = serde_json::from_slice::<petri_nats::NetMetadata>(&entry) {
                        if meta.status == petri_nats::NetStatus::Completed
                            || meta.status == petri_nats::NetStatus::Cancelled
                        {
                            // TERMINAL: a completed/cancelled net will never accept the
                            // token. Signal Terminal so the loop dead-letters it instead
                            // of NACKing forever (which otherwise spams the bridge loop).
                            return Err(BridgeResolveError::Terminal(format!(
                                "Net '{}' is {:?} — cannot accept bridge tokens",
                                net_id, meta.status
                            )));
                        }
                    }
                }
                Ok(None) => {
                    // NOT-READY: the net may simply not have been created yet (spawn
                    // race). Retry via redelivery.
                    return Err(BridgeResolveError::NotReady(format!(
                        "Net '{}' unknown — no metadata entry found",
                        net_id
                    )));
                }
                Err(e) => {
                    // Transient KV hiccup — retry.
                    return Err(BridgeResolveError::NotReady(format!(
                        "Net '{}' metadata lookup failed: {}",
                        net_id, e
                    )));
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
    service: Arc<
        petri_application::PetriNetService<
            NatsEventStore<MemoryEventStore>,
            MemoryTopologyStore,
            MarkingProjection,
        >,
    >,
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
            // The net was resolved but its topology isn't loaded yet — almost
            // always because `resolve_net` triggered an ASYNC wake from
            // hibernation and this inject raced it. Transient: NACK + redeliver
            // (NOT `Other`, which would dead-letter and silently drop the token
            // — e.g. a runner-pool release/claim landing on a hibernated
            // `pool-*` net, stranding the lease forever).
            Err(petri_application::ServiceError::NoTopology) => {
                Err(petri_nats::BridgeInjectError::NotReady)
            }
            Err(e) => Err(petri_nats::BridgeInjectError::Other(e.to_string())),
        }
    }

    fn notify_eval(&self) {
        self.eval_notify.notify_one();
    }
}

/// Implements `NetCreator` for the `NetRegistry` (creates nets via NATS command).
struct RegistryNetCreator {
    registry:
        Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
    activity: Option<Arc<ActivityTracker>>,
}

#[async_trait::async_trait]
impl petri_nats::NetCreator for RegistryNetCreator {
    async fn create_and_load(
        &self,
        request: &petri_nats::CreateNetRequest,
        workspace: &str,
    ) -> Result<(), String> {
        let instance = self.registry.get_or_create(&request.net_id);

        // Multi-tenancy linchpin / hazard #3: stamp the workspace recovered from
        // the create-net subject and START the deferred event consumer BEFORE the
        // first append (`NetCreated` below + `initialize`'s `NetInitialized`).
        // Both go through the NATS publisher, which reads the shared workspace
        // cell to pick the subject and BLOCKS waiting for the consumer to apply
        // the event; the consumer must be live AND filtered on the real
        // workspace before that. Without this, spawned / NATS-created child nets
        // publish under DEFAULT_WORKSPACE regardless of the parent's tenant.
        instance.service.set_workspace_id(workspace.to_string());
        instance.start_event_consumer().await;

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
        // The scenario JSON is the sub-phase 2.5e-γ.mekhan LoadScenarioRequest
        // envelope shape: { scenario, skip_mask?, stage_overrides? }. Bare
        // ScenarioDefinition input is also tolerated here via a fallback
        // deserialize, since this code path (NATS dispatch CLI driver) loads
        // scenarios from on-disk JSON files that may pre-date the envelope.
        let envelope: petri_api::dto::LoadScenarioRequest =
            serde_json::from_value(request.scenario.clone())
                .or_else(|_envelope_err| {
                    // Fallback: parse as bare ScenarioDefinition for CLI/file-based
                    // dispatch (envelope wire shape is cloud-layer-only). Tracked by
                    // workstream #110 (envelope-uniform CLI loader).
                    serde_json::from_value::<petri_api::dto::ScenarioDefinition>(
                        request.scenario.clone(),
                    )
                    .map(petri_api::dto::LoadScenarioRequest::from_scenario)
                })
                .map_err(|e| format!("Invalid scenario JSON: {}", e))?;

        let scenario = envelope.into_scenario();

        // AIR version gate — same contract as the HTTP load path: refuse to
        // interpret a definition emitted for a newer AIR format.
        if scenario.air_version > petri_api::dto::SUPPORTED_AIR_VERSION {
            return Err(format!(
                "unsupported AIR version: scenario declares air_version {}, this engine supports <= {}. \
                 Upgrade the engine, or re-compile the workflow against this engine's AIR format.",
                scenario.air_version,
                petri_api::dto::SUPPORTED_AIR_VERSION
            ));
        }

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

    // Dead-letter stream for terminally-failed listener messages
    match jetstream
        .get_or_create_stream(petri_nats::dlq_stream_config())
        .await
    {
        Ok(_) => info!(name = %Subjects::STREAM_DLQ, "DLQ stream ready"),
        Err(e) => {
            tracing::warn!(error = %e, "Could not create DLQ stream");
        }
    }

    // Create human task streams (cancel, cancelled, failed)
    // HUMAN_REQUESTS and HUMAN_COMPLETED are created by the human client and UI respectively.
    {
        // These are single global streams capturing the human subjects of ALL
        // workspaces. Human subjects are ws-segmented
        // (`human.{ws}.{category}.{net}.{place}`), so each stream captures a
        // `human.*.{category}.>` wildcard across the workspace token.
        // TODO(stream-per-ws): shard these streams per-workspace.
        let human_streams = [
            (
                Subjects::STREAM_HUMAN_CANCEL,
                Subjects::HUMAN_CANCEL_CATEGORY,
            ),
            (
                Subjects::STREAM_HUMAN_CANCELLED,
                Subjects::HUMAN_CANCELLED_CATEGORY,
            ),
            (
                Subjects::STREAM_HUMAN_FAILED,
                Subjects::HUMAN_FAILED_CATEGORY,
            ),
        ];
        for (stream_name, category) in human_streams {
            match jetstream
                .get_or_create_stream(StreamConfig {
                    name: stream_name.to_string(),
                    subjects: vec![format!("{}.*.{}.>", Subjects::HUMAN_ROOT, category)],
                    retention: RetentionPolicy::Limits,
                    max_age: Duration::from_secs(7 * 24 * 60 * 60),
                    ..Default::default()
                })
                .await
            {
                Ok(_) => info!(name = %stream_name, "Human stream ready"),
                Err(e) => {
                    tracing::warn!(error = %e, name = %stream_name, "Could not create human stream")
                }
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
    registry:
        Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
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
    registry:
        Arc<NetRegistry<NatsEventStore<MemoryEventStore>, MemoryTopologyStore, MarkingProjection>>,
    metadata_kv: async_nats::jetstream::kv::Store,
    activity_tracker: Option<Arc<ActivityTracker>>,
}

/// DELETE /api/nets/{net_id} — properly terminate and clean up a net.
///
/// Handles three cases:
/// 1. Hot net (in registry): terminate with lifecycle event + cancel tasks.
/// 2. Hibernated-but-active net (in KV with status Running/Created, but evicted
///    from the registry): REHYDRATE it into a hot net, then terminate through the
///    exact same path as Case 1.
/// 3. Already terminal (completed/cancelled) or genuinely unknown: no-op cleanup
///    or 404.
///
/// ## Why a hibernated active net MUST be rehydrated before cancel
///
/// `NetRegistry::terminate` runs `service.drain_finalizers()` BEFORE emitting
/// `NetCancelled`, firing any `t_<id>_finally` finalizer to release a held lease.
/// A leased net's success-path release (`t_<id>_exit`) is gated on the body
/// completing, so a mid-run cancel never releases — the single held token sits in
/// the pool net's `in_use` place forever (event-sourced → survives restart →
/// permanently strands the runner/allocation). That is the strand bug fixed for
/// HOT nets in 922dd9b4.
///
/// The drain can only release what it can SEE: it scans the in-memory marking for
/// the parked held token. A hibernated net has been evicted from the registry, so
/// its marking is not in memory. The previous Case 2 published `NetCancelled`
/// straight to NATS WITHOUT rehydrating and WITHOUT draining — so a hibernated
/// leased net skipped the finalizer entirely and re-stranded its lease.
///
/// The fix: call `get_or_create(&net_id)` to rehydrate. The store factory BLOCKS
/// on hydration (`ready_rx.await`) before returning the stores, replaying the
/// net's full NATS event log into the in-memory cache — so the reconstructed
/// marking (including the parked held-lease token) is present. A woken net
/// resumes in `RunMode::Running`, exactly as `GlobalSignalListener` wakes it.
/// Once hot, the Case-1 block (which re-checks `get(&net_id).is_some()`) handles
/// it uniformly: the datacenter pre-terminate hook + `terminate` → the finalizer
/// drain sees the held token and journals the release ahead of `NetCancelled`.
///
/// Only Running/Created nets are rehydrated — never completed/cancelled/tombstoned
/// nets, which have no held token to release and must not be resurrected.
async fn delete_net_handler(
    axum::extract::State(state): axum::extract::State<NetDeletionState>,
    axum::extract::Path(net_id): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    // Read the metadata KV ONCE up front. We need it both to decide whether a
    // cold net is still active (→ rehydrate) and to drive the terminal/404 arms.
    let meta = match state.metadata_kv.get(&net_id).await {
        Ok(Some(entry)) => serde_json::from_slice::<NetMetadata>(&entry).ok(),
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(net_id = %net_id, error = %e, "Failed to read metadata KV");
            None
        }
    };

    // Rehydrate a hibernated-but-active net so the finalizer drain below can find
    // the parked held-lease token (see the doc comment above). After this, the
    // Case-1 block handles it uniformly. We only rehydrate active nets, and only
    // when they are not already hot, so this is a no-op for hot/terminal nets.
    if state.registry.get(&net_id).is_none()
        && matches!(
            meta.as_ref().map(|m| &m.status),
            Some(NetStatus::Running) | Some(NetStatus::Created)
        )
    {
        tracing::info!(
            net_id = %net_id,
            "Rehydrating hibernated active net before cancel (so finalizer drain can \
             release any held lease)"
        );
        // `get_or_create` is a sync fn; it internally uses `block_in_place` for
        // the hydration wait — calling it from this async handler is fine (it is
        // how the NATS signal listeners wake hibernated nets).
        let _ = state.registry.get_or_create(&net_id);
    }

    // Case 1: Hot net (now includes a just-rehydrated hibernated active net) —
    // terminate properly (drains finalizers, emits NetCancelled, cancels tasks).
    if state.registry.get(&net_id).is_some() {
        // docs/16 §8 — pre-terminate hook: release any cluster lease HELD on
        // behalf of this instance BEFORE we tear down the eval loop. `terminate`
        // hibernates the loop before it can reach its natural `t_release`, so
        // without this the held salloc / dispatched drain executor + the cluster
        // watcher + SSH socket would leak. The release is best-effort + idempotent
        // (404-tolerant), so a double-cancel or a cancel racing a natural release
        // is harmless.
        #[cfg(any(feature = "slurm", feature = "nomad"))]
        state
            .registry
            .release_held_leases_for_instance(&net_id)
            .await;
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
                tracing::info!(net_id = %net_id, "Net deleted (was hot or rehydrated)");
                return StatusCode::NO_CONTENT.into_response();
            }
            // The net vanished between rehydrate/hot-check and terminate (a race
            // with a natural completion or a concurrent cancel). `terminate`
            // returns `Err("Net '<id>' not found")` in that case — treat it as
            // idempotent success, matching the rest of this handler's philosophy.
            Err(e) if e.contains("not found") => {
                if let Some(ref activity) = state.activity_tracker {
                    let _ = activity.remove(&net_id).await;
                }
                tracing::info!(net_id = %net_id, "Net already gone at terminate (idempotent)");
                return StatusCode::NO_CONTENT.into_response();
            }
            Err(e) => {
                tracing::error!(net_id = %net_id, error = %e, "Failed to terminate net");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({"error": format!("Failed to terminate: {}", e)})),
                )
                    .into_response();
            }
        }
    }

    // Not hot (and not an active net we could rehydrate). Resolve via the
    // metadata KV read at the top of the handler.
    match meta {
        // An active net that we failed to make hot above would have been caught
        // by the Case-1 block; reaching here with an active status means the net
        // could not be rehydrated (vanished) — idempotent no-op cleanup.
        Some(_) => {
            // Already completed/cancelled (or raced away) — clean up any leftover
            // activity entry.
            if let Some(ref activity) = state.activity_tracker {
                let _ = activity.remove(&net_id).await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": "Net not found"})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod woken_workspace_resolver_tests {
    use super::woken_workspace_from_metadata;
    use petri_nats::Subjects;

    /// Build a minimal serialized `NetMetadata` blob carrying `workspace_id`.
    fn meta_blob(workspace_id: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "net_id": "pool-5f27e605",
            "status": "running",
            "workspace_id": workspace_id,
            "created_at": "2026-06-22T00:00:00Z",
        }))
        .unwrap()
    }

    /// REGRESSION: a net the projection stamped under the DEFAULT workspace must
    /// resolve to `Some("default")`, NOT `None`. The original code folded the
    /// default workspace into `None`, which made the woken-net branch of
    /// `get_or_create` skip starting the event consumer entirely — so topology
    /// never hydrated and a bridged-to capacity pool failed activation with
    /// `BRIDGE_TARGET_NET_MISSING`.
    #[test]
    fn default_workspace_resolves_to_some_not_none() {
        let blob = meta_blob(Subjects::DEFAULT_WORKSPACE);
        assert_eq!(
            woken_workspace_from_metadata(Some(&blob)),
            Some(Subjects::DEFAULT_WORKSPACE.to_string()),
            "a default-recorded net must wake under 'default', not be treated as unknown"
        );
    }

    /// A multi-tenant net resolves to its recorded tenant workspace verbatim.
    #[test]
    fn tenant_workspace_resolves_verbatim() {
        let blob = meta_blob("9a1d2bf1");
        assert_eq!(
            woken_workspace_from_metadata(Some(&blob)),
            Some("9a1d2bf1".to_string())
        );
    }

    /// No metadata entry → `None` → the registry DEFERS consumer start to
    /// `load_scenario` (the genuinely-fresh-net path). This is the ONLY case
    /// that may return `None`.
    #[test]
    fn missing_entry_is_none() {
        assert_eq!(woken_workspace_from_metadata(None), None);
    }

    /// An unparseable blob is treated as "no usable entry" → `None`.
    #[test]
    fn garbage_entry_is_none() {
        assert_eq!(woken_workspace_from_metadata(Some(b"not json")), None);
    }
}
