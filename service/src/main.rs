use std::net::SocketAddr;
use std::sync::Arc;

use mekhan_service::auth::authenticator::{Authenticator, BffAuthenticator, NoopAuthenticator};
use mekhan_service::auth::bff::oidc::{OidcClient, OidcConfig};
use mekhan_service::auth::bff::session::{PgSessionStore, SessionStore};
use mekhan_service::auth::dev::NoopTokenVerifier;
use mekhan_service::auth::resolver::DbPrincipalResolver;
use mekhan_service::auth::zitadel::{ZitadelConfig, ZitadelTokenVerifier};
use mekhan_service::auth::{IntrospectionVerifier, PrincipalResolver, TokenVerifier, ZitadelMgmt};
use mekhan_service::catalogue::repository::PgCatalogueRepository;
use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::config::{AppConfig, AuthMode};
use mekhan_service::db;
use mekhan_service::lifecycle;
use mekhan_service::nats::MekhanNats;
use mekhan_service::petri::client::PetriClient;
use mekhan_service::s3::ArtifactStore;
use mekhan_service::yjs::manager::YjsManager;
use mekhan_service::yjs::persistence::YjsPersistence;
use mekhan_service::{build_router, AppState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mekhan_service=info,tower_http=info".into()),
        )
        .init();

    let config = AppConfig::load().expect("failed to load configuration");
    tracing::info!("starting mekhan-service on {}:{}", config.host, config.port);

    let db = db::create_pool(&config.database_url).await?;
    tracing::info!("database connected and migrations applied");

    let petri = PetriClient::new(&config.petri_lab_url);

    let mekhan_nats = MekhanNats::connect(&config.nats_url, config.nats_creds.as_deref()).await?;
    tracing::info!("NATS connected at {}", config.nats_url);

    // Silent-drop DLQ wiring: ensure the JetStream stream + spawn the
    // background drainer. After this point, every `record_silent_drop*`
    // call also publishes a `SilentDropRecord` to MEKHAN_SILENT_DROPS,
    // queryable via `GET /api/v1/observability/silent-drops`.
    mekhan_nats
        .ensure_silent_drops_stream()
        .await
        .expect("failed to create MEKHAN_SILENT_DROPS stream");

    // Lab-runner-fleet dead-letter stream is cluster-owned: create it here so an
    // enrolled (consumer-only) runner never has to — its scoped JWT stays
    // STREAM.INFO-only on the shared `runner-jobs_dlq`. Non-fatal: a runner
    // re-mint can still self-heal if this races a cold NATS, and the in-cluster
    // worker path is unaffected.
    if let Err(e) = mekhan_nats.ensure_runner_jobs_dlq_stream().await {
        tracing::warn!(error = %e, "could not ensure runner-jobs_dlq stream at startup");
    }
    if let Some(drain_rx) = mekhan_service::observability::install_drainer() {
        let drain_nats = mekhan_nats.clone();
        tokio::spawn(async move {
            mekhan_service::observability::drain_silent_drops(drain_nats, drain_rx).await;
        });
        tracing::info!("silent drop drainer started");
    }

    // Create catalogue subscription manager (KV-backed, in-memory cached)
    let sub_kv = mekhan_nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("failed to create CATALOGUE_SUBSCRIPTIONS KV bucket");
    let subscription_manager = Arc::new(SubscriptionManager::new(
        sub_kv,
        mekhan_nats.jetstream().clone(),
        db.clone(),
    ));
    subscription_manager
        .hydrate()
        .await
        .expect("failed to hydrate catalogue subscriptions");
    subscription_manager
        .clone()
        .start_watcher()
        .await
        .expect("failed to start catalogue subscription watcher");
    tracing::info!("catalogue subscription manager ready");

    // Spawn lifecycle event listener (updates DB on NetCompleted/NetCancelled).
    // Triggers are wired in later once the dispatcher is built — see below.

    let artifact_store = Arc::new(ArtifactStore::new(&config.s3));
    if let Err(e) = artifact_store.ensure_bucket().await {
        tracing::warn!("S3 bucket check failed (non-fatal): {e}");
    } else {
        tracing::info!("S3 artifact store ready (bucket: {})", config.s3.bucket);
    }

    // Spawn background cleanup sweep (also GCs per-instance agent transcript
    // blobs from the artifact store on the retention sweep).
    tokio::spawn(lifecycle::start_cleanup_sweep(
        config.cleanup.clone(),
        db.clone(),
        mekhan_nats.clone(),
        petri.clone(),
        artifact_store.clone(),
    ));

    // File-analytics growth snapshots (docs/32 Cut 2): periodic aggregate
    // captures of file_inventory into the inventory_snapshots hypertable.
    // The manual POST /api/v1/data/analytics/snapshot trigger shares the
    // same writer regardless of this switch.
    if config.analytics.snapshot_enabled {
        tokio::spawn(mekhan_service::analytics::snapshot::start_snapshot_job(
            config.analytics.clone(),
            db.clone(),
        ));
    }

    let yjs_persistence = YjsPersistence::new(db.clone());
    let yjs_manager = Arc::new(YjsManager::new(yjs_persistence));
    tracing::info!("Yjs collaboration manager initialized");

    let artifact_s3 = config
        .artifact_s3
        .as_ref()
        .map(|cfg| Arc::new(ArtifactStore::new(cfg)));

    // Live broadcasts for SSE fan-out of metric/log events.
    let live = mekhan_service::causality::live::LiveBroadcasts::new();

    // Trigger dispatcher — hydrates from every published template's
    // graph_json on boot. Background sources (cron, catalog, lifecycle,
    // webhook) hang off the same dispatcher in subsequent sub-phases.
    // Created before the lifecycle listener + causality ingest so we can
    // hand it through; the ingest hook fires catalog triggers on new
    // artifacts and the lifecycle hook fires net_completion triggers.
    let trigger_dispatcher = mekhan_service::triggers::start_trigger_dispatcher(
        db.clone(),
        petri.clone(),
        mekhan_nats.clone(),
    )
    .await;
    tracing::info!("trigger dispatcher ready");

    // WaitForResult waiter registry — shared between the fire handler (via
    // AppState) and the lifecycle consumer that resolves terminal outcomes.
    let result_waiters = mekhan_service::triggers::ResultWaiters::new();

    // One-time per-template usage rollup backfill (migration 20240175). Awaited
    // BEFORE the lifecycle listener + step-executions projector start so the
    // full TRUNCATE+rebuild from the durable source tables can't race the
    // incremental maintainers (which then resume folding new terminals on top).
    // Self-guarded: skips once the rollups hold any row, so it only runs on the
    // first boot after the migration. Best-effort — never fatal.
    mekhan_service::analytics::template::backfill_template_rollups_if_empty(db.clone()).await;

    tokio::spawn(lifecycle::start_lifecycle_listener(
        mekhan_nats.clone(),
        db.clone(),
        subscription_manager.clone(),
        Some(trigger_dispatcher.clone()),
        result_waiters.clone(),
    ));

    // Causality ingest (PETRI_GLOBAL domain events → causality tables)
    // Single projection path for processes, tasks, metrics, logs, and catalogue.
    // The bucket workflow artifacts physically land in — used as the
    // `file_server_id` for the platform's own object store when the causality
    // projector couples a catalogued artifact to its physical-copy inventory
    // row. Mirrors the artifact-store selection (artifact_s3 overrides s3).
    let object_store_id = config
        .artifact_s3
        .as_ref()
        .map(|c| c.bucket.clone())
        .unwrap_or_else(|| config.s3.bucket.clone());
    tokio::spawn(mekhan_service::causality::ingest::start_causality_ingest(
        mekhan_nats.clone(),
        db.clone(),
        subscription_manager.clone(),
        live.clone(),
        Some(trigger_dispatcher.clone()),
        object_store_id,
    ));

    // Inventory fold ingest (docs/32 batch-fold): sink-mode crawl batches →
    // set-based file_inventory upserts (+ catalogue coupling on hash). The
    // scale path for multi-million-file campaigns — per-file rows never touch
    // the engine or the causality projector.
    tokio::spawn(
        mekhan_service::inventory::fold::start_inventory_fold_ingest(
            mekhan_nats.clone(),
            db.clone(),
        ),
    );

    // Step-executions projection (PETRI_GLOBAL domain events → step_execution
    // table). Folds per-step inputs/outputs/metrics for the instance-view
    // canvas overlay.
    tokio::spawn(
        mekhan_service::projections::step_executions::start_step_executions_ingest(
            mekhan_nats.clone(),
            db.clone(),
        ),
    );

    // Allocations projection (PETRI_GLOBAL domain events → allocations table).
    // Folds resource-lease acquire/release effects + accounting signals into
    // per-grant rows for the control-plane allocations / instance overlays.
    tokio::spawn(
        mekhan_service::projections::allocations::start_allocations_ingest(
            mekhan_nats.clone(),
            db.clone(),
        ),
    );

    // Template-stagings projection (PETRI_GLOBAL → template_stagings). Folds each
    // generated staging net's terminal `stage_template` effect into its staging
    // row's status/remote_ref/last_error (B-staging, Phase 4).
    tokio::spawn(
        mekhan_service::projections::template_stagings::start_template_stagings_ingest(
            mekhan_nats.clone(),
            db.clone(),
        ),
    );

    // Image-materializations projection (PETRI_GLOBAL → image_materializations).
    // Folds each materialize net's terminal `materialize_image` effect into its
    // row's status/digest/sif_path/last_error (docs/22 container staging).
    tokio::spawn(
        mekhan_service::projections::image_materializations::start_image_materializations_ingest(
            mekhan_nats.clone(),
            db.clone(),
        ),
    );

    // Engine-initiated human task cancellations. When a Timeout's timer wins
    // the SLA race, the engine fires `human_cancel` and publishes to
    // `human.cancel.{net_id}.{place}` — without this listener, hpi_tasks
    // would stay `pending` forever even though the engine moved on.
    tokio::spawn(
        mekhan_service::process::cancel_listener::start_human_cancel_listener(
            mekhan_nats.clone(),
            db.clone(),
        ),
    );

    // Unified fleet-liveness registry (docs/24 S1) — the shared telemetry plane
    // over BOTH the anonymous worker pool and the advisory facet of enrolled
    // runners. Construct it ONCE: the worker subscriber/sweep mutate it, the
    // presence controller mirrors each runner's advertised backends into it, and
    // the AppState publish-time warning reads through it. Purely advisory — it
    // never reaps an instance.
    let fleet = mekhan_service::fleet::FleetLiveness::new();

    // Presence controller (Phase 3 — presence-lease pool capacity). Subscribes
    // to `runner.*.presence` and drives every `runner_group` net: admits one
    // pool unit per live runner via the `presence_acquire` bridge on the
    // absent→present edge, and a background sweep injects a bare
    // `presence_expired` signal on a TTL miss. NATS is always connected here
    // (mekhan can't boot without it), so this is unconditional like the other
    // NATS-backed background tasks.
    // Construct the shared presence handle ONCE: the controller tasks mutate it
    // and the AppState read API (`GET /api/v1/runners/presence`) reads through it.
    // The `fleet` clone receives the advisory backend mirror (telemetry only —
    // NOT the pool-net control binding, which stays inside this controller).
    let runner_presence = mekhan_service::presence::RunnerPresence::new();
    mekhan_service::presence::spawn_presence_controller(
        runner_presence.clone(),
        mekhan_nats.clone(),
        db.clone(),
        petri.clone(),
        fleet.clone(),
    );

    // Human presence controller (docs/33 §7 — humans as a capacity). The human
    // analogue of the runner presence controller: a roster MEMBER's availability
    // (not a daemon heartbeat) drives admission into the `pool-<capacity_id>`
    // net. Subscribes to BOTH `human.*.availability` (durable intent toggle) and
    // `human.*.presence` (session/external liveness), and a sweep reaps a
    // TTL-missing member. The injected pool unit reuses the runner plumbing
    // verbatim, so no engine HTTP client is needed (pure NATS bridge + signal).
    // Construct the shared handle ONCE: the controller tasks mutate it and the
    // AppState read API reads through it.
    let human_presence = mekhan_service::presence::HumanPresence::new();
    mekhan_service::presence::spawn_human_presence_controller(
        human_presence.clone(),
        mekhan_nats.clone(),
        db.clone(),
        petri.clone(),
    );

    // Worker liveness tasks (worker-pool feature). Subscribe to
    // `worker.*.presence` and keep a TTL-swept set of which `ExecutorJob`
    // backends have ≥1 live worker, so publish can WARN (never fail) when a
    // step's backend is served by zero capacities. The `db` handle lets the
    // subscriber reflect each enrolled worker's heartbeat into `workers.last_seen_at`
    // (the executor only emits NATS presence, never the HTTP heartbeat). Share the
    // `fleet` handle stored in AppState.
    mekhan_service::fleet::spawn_worker_liveness(fleet.clone(), mekhan_nats.clone(), db.clone());

    // Model-pool placement autoscaler. A reconcile loop that decides WHICH models
    // are loaded and how they are spread across the already-registered LLM runners,
    // publishing NATS load/unload to enrolled runners (no node provisioning).
    // Inference never touches the engine net or the presence net.
    //
    // `AUTOSCALER_DEMAND_URL` (the Router base) flips it from L1 manual mode to
    // L2 reactive: a `PrometheusDemandSource` scrapes the router `/metrics`
    // per-model demand so `scale_to_zero`/`keep_warm` policies react to load.
    // Unset ⇒ L1 (`demand = None`, only `manual` policies place).
    let demand: Option<std::sync::Arc<dyn mekhan_service::autoscaler::demand::DemandSource>> =
        std::env::var("AUTOSCALER_DEMAND_URL")
            .ok()
            .filter(|u| !u.is_empty())
            .map(|url| {
                tracing::info!(%url, "autoscaler L2 reactive mode: scraping router /metrics");
                std::sync::Arc::new(
                    mekhan_service::autoscaler::demand::PrometheusDemandSource::new(url),
                )
                    as std::sync::Arc<dyn mekhan_service::autoscaler::demand::DemandSource>
            });
    mekhan_service::autoscaler::spawn_autoscaler(
        db.clone(),
        mekhan_nats.clone(),
        runner_presence.clone(),
        demand,
    );

    // Inference-metering audit ledger (INFERENCE_METERING → inference_request_log).
    // The router publishes one complete InferenceRequestLog per request; this
    // projector upserts each idempotently keyed by request_id (model-pool P5).
    tokio::spawn(
        mekhan_service::projections::inference_metering::start_inference_metering_ingest(
            mekhan_nats.clone(),
            db.clone(),
        ),
    );

    let catalogue_repo = Arc::new(PgCatalogueRepository::new(db.clone()));

    // Spawn catalogue NATS request-reply responder
    tokio::spawn(
        mekhan_service::catalogue::responder::start_catalogue_responder(
            mekhan_nats.clone(),
            catalogue_repo.clone(),
            subscription_manager.clone(),
        ),
    );

    // Auth adapters — composition root chooses the implementation by config.
    // `TokenVerifier`/`PrincipalResolver` are reused unchanged: the BFF
    // callback verifies the IdP's token with them, then caches the resolved
    // `AuthUser`. The per-request hot path goes through the `Authenticator`.
    let token_verifier = build_token_verifier(&config).await?;
    // `DbPrincipalResolver` enriches the static claim mapping with a
    // workspace lookup against `workspaces`/`workspace_members`, so every
    // resolved `AuthUser` carries a `workspace_id`. Tests keep using
    // `StaticPrincipalResolver` (no DB) — that path yields `workspace_id =
    // None` which handlers tolerate by falling back to the default
    // workspace at the call site.
    // `auth.multi_org` (default false) gates real per-org tenancy: each Zitadel
    // org claim maps to its bound workspace, and the legacy auto-join-`default`-
    // as-editor fallback is dropped. `auth.auto_join_system_workspaces` (default
    // false) gates the legacy `demos`-as-viewer auto-join; `auth.platform_admins`
    // sets `is_platform_admin`. The resolver never auto-joins the shared
    // `default` tenant in either mode (a homeless principal gets a personal
    // workspace instead).
    let principal_resolver: Arc<dyn PrincipalResolver> = Arc::new(DbPrincipalResolver::with_options(
        db.clone(),
        config.auth.multi_org,
        config.auth.auto_join_system_workspaces,
        config.auth.platform_admins.clone(),
    ));

    let session_store: Arc<dyn SessionStore> = Arc::new(PgSessionStore::new(db.clone()));
    let (authenticator, oidc) = build_authenticator(&config, session_store.clone()).await?;
    let introspection = build_introspection(&config).await?;
    let zitadel_mgmt = build_zitadel_mgmt(&config)?;

    // Background sweep of expired sessions + stale login flows.
    {
        let store = session_store.clone();
        let ttl = config.auth.session_ttl_secs;
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                tick.tick().await;
                match store.sweep_expired(ttl).await {
                    Ok(n) if n > 0 => {
                        tracing::info!("auth: swept {n} expired session/login rows")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!("auth: session sweep failed: {e}"),
                }
            }
        });
    }

    // Phase B.9 — write-side secret store for the Resource CRUD handlers.
    // `VAULT_ADDR` + `VAULT_TOKEN` set → real Vault; either missing → an
    // in-memory placeholder + a WARN so operators notice before secrets
    // start landing in process memory in prod.
    let resource_store: Arc<dyn aithericon_resources::ResourceSecretStore> =
        match aithericon_resources::VaultResourceStore::from_env() {
            Some(vrs) => {
                tracing::info!("resource_store: Vault-backed (VAULT_ADDR set)");
                Arc::new(vrs)
            }
            None => {
                tracing::warn!(
                    "resource_store: VAULT_ADDR/VAULT_TOKEN unset — falling back to \
                     in-memory store. Resource secrets WILL NOT SURVIVE A RESTART. \
                     Configure Vault before production deployments."
                );
                Arc::new(aithericon_resources::InMemoryResourceStore::new())
            }
        };

    // READ-side secret store for the file-server serve bridge (docs/32). Same
    // Vault-or-fallback posture as `resource_store`: a real `VaultSecretStore`
    // when VAULT_ADDR/VAULT_TOKEN are set, an empty `InMemorySecretStore`
    // otherwise. With the in-memory fallback, serving an external s3/sftp
    // endpoint fails cleanly at credential-read time (the secret isn't there),
    // never silently — the built-in object_store path needs no creds and is
    // unaffected.
    let secret_store: Arc<dyn aithericon_secrets::SecretStore> =
        match aithericon_secrets::VaultSecretStore::from_env() {
            Some(vss) => {
                tracing::info!("secret_store: Vault-backed (VAULT_ADDR set)");
                Arc::new(vss)
            }
            None => {
                tracing::warn!(
                    "secret_store: VAULT_ADDR/VAULT_TOKEN unset — falling back to \
                     empty in-memory store. Serving external s3/sftp file-server \
                     endpoints (resource_ref creds) will fail until Vault is configured."
                );
                Arc::new(aithericon_secrets::InMemorySecretStore::new(
                    std::collections::HashMap::new(),
                ))
            }
        };

    // Publish-time resource resolver. Stateless on construction — every call
    // joins workspace + version + ACL inline. Shared as `Arc` so the publish
    // path can clone it cheaply.
    let resource_resolver =
        Arc::new(mekhan_service::petri::resource_resolver::ResourceResolver::new(db.clone()));

    // Phase 2 (Lab Runner Fleet) — resolve the NATS account signing key. Logs
    // the account public key at INFO. Precedence: RUNNERS_NATS_SIGNING_SEED env
    // → local seed file ({MEKHAN_DATA_DIR|~/.aithericon/mekhan}) → best-effort
    // Vault → generate+persist. Never blocks startup on Vault.
    let runner_nats_signer = Arc::new(mekhan_service::runners_nats::RunnerNatsSigner::resolve());
    tracing::info!(
        account = %runner_nats_signer.account_public_key(),
        "runner NATS signer ready"
    );
    // Asset resolver — publish-time materialization of node-bound asset records
    // into the spliced `__assets` envelope (docs/20 §5). Same `Arc`-shared
    // shape as `resource_resolver`.
    let asset_resolver = Arc::new(mekhan_service::petri::asset_resolver::AssetResolver::new(
        db.clone(),
    ));

    // Invite (Phase 4) delivery + identity provisioning seams. Email defaults to
    // log-mode (offline); the provisioner is the deterministic Noop under
    // dev_noop and the real Zitadel broker under any auth mode. The boot
    // invariant rejects a synthetic provisioner under a real auth mode.
    let email = mekhan_service::notify::email::build_mailer(&config);
    let user_provisioner = mekhan_service::auth::provisioner::build_user_provisioner(&config);
    mekhan_service::auth::provisioner::assert_provisioner_invariant(
        config.auth.mode,
        &user_provisioner,
    );

    let state = AppState {
        db,
        petri,
        nats: mekhan_nats,
        config: config.clone(),
        yjs: yjs_manager,
        s3: artifact_store,
        artifact_s3,
        catalogue_repo,
        live,
        authenticator,
        session_store,
        oidc,
        token_verifier,
        principal_resolver,
        introspection,
        zitadel_mgmt,
        triggers: trigger_dispatcher,
        result_waiters,
        resource_store,
        secret_store,
        resource_resolver,
        runner_nats_signer,
        runner_presence,
        human_presence,
        fleet,
        asset_resolver,
        email,
        user_provisioner,
    };

    // Close the dispatcher's forward reference to `AppState` (the dispatcher was
    // built before `AppState`, which holds `Arc<TriggerDispatcher>`). This lets
    // trigger-fired launches route through the binding-aware launcher
    // (`prepare_air_with_bindings`) — per-workspace defaults / platform auto-bind
    // / the run-gate — exactly like the user POST path. `AppState` is cheap to
    // clone (every field is Arc/handle-shaped); the resulting reference cycle is
    // intentional and benign for this boot-lifetime singleton.
    state.triggers.set_app_state(state.clone());

    // Unified worker dispatch: the SINGLE platform-tier "default" worker group (a
    // `capacity` resource, `worker` preset, owned by PLATFORM_SCOPE_ID) so a step
    // / worker that names no group routes through the shared competing-consumer
    // pool. The executor queue is already a global data plane (no lease/bridge
    // net), so its control-plane routing partition lives at the platform tier,
    // not per workspace. Seed it before the listener accepts requests. Idempotent
    // + best-effort.
    if let Err(e) =
        mekhan_service::worker_groups::ensure_platform_default_worker_group(&state).await
    {
        tracing::warn!(error = ?e, "platform default worker-group seed failed");
    }

    // Model pool: the SINGLE platform-tier `model_serving` runner group (a
    // `capacity` resource, `instrument` preset, owned by PLATFORM_SCOPE_ID) so
    // self-hosted LLM serving nodes have a first-class shared pool to enrol into
    // — group membership, not a `base_url` sniff, identifies a runner as part of
    // the LLM pool. The inference data plane is already cluster-wide, so its
    // control-plane membership lives at the platform tier, not per workspace.
    // Idempotent + best-effort.
    if let Err(e) = mekhan_service::model_serving_group::ensure_platform_model_serving_group(&state).await {
        tracing::warn!(error = ?e, "platform model-serving-group seed failed");
    }

    // Config-seeded platform bootstrap registration tokens (MEKHAN__BOOTSTRAP__*):
    // upsert reusable, platform-scoped worker/runner registration tokens so the
    // executor + model-pool runners self-enroll declaratively (no interactive
    // mint). Runs AFTER the group seeders (the tokens reference those groups).
    // No-op when unset; best-effort (a malformed token logs a warning).
    if let Err(e) = mekhan_service::bootstrap::ensure_bootstrap_worker_token(&state).await {
        tracing::warn!(error = %e, "bootstrap worker registration-token seed failed");
    }
    if let Err(e) = mekhan_service::bootstrap::ensure_bootstrap_runner_token(&state).await {
        tracing::warn!(error = %e, "bootstrap runner registration-token seed failed");
    }

    // Seed built-in demos before the listener accepts requests. Idempotent
    // by stable template id (see `mekhan_service::demos`); best-effort —
    // a failure to seed logs a warning and is otherwise transparent. Gated
    // by `demos.seed` so production deployments must opt in.
    if config.demos.seed {
        let demos_dir = std::path::PathBuf::from(&config.demos.dir);
        match mekhan_service::demos::seed_all(&state, &demos_dir).await {
            Ok(outcomes) => {
                let seeded = outcomes
                    .iter()
                    .filter(|(_, o)| matches!(o, mekhan_service::demos::SeedOutcome::Seeded))
                    .count();
                let already = outcomes.len() - seeded;
                tracing::info!(
                    demos_dir = %demos_dir.display(),
                    seeded,
                    already_present = already,
                    "demo seeder finished"
                );
            }
            Err(e) => {
                tracing::warn!(error = %e, "demo seeder failed — proceeding without demos");
            }
        }
    }

    // Seed the built-in platform object store as a first-class file server
    // (docs/32 §4.1) so every `log_artifact` copy lands on a real, tracked
    // backend (key = the platform S3 bucket; no resource_ref — uses platform
    // config). Idempotent + best-effort; runs regardless of the demo seeder.
    if let Err(e) = mekhan_service::file_servers::queries::seed_builtin_object_store(
        &state.db,
        uuid::Uuid::nil(),
        &config.s3.bucket,
    )
    .await
    {
        tracing::warn!(error = %e, "file-server object-store seed failed — proceeding");
    }

    let app = build_router(state);

    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    tracing::info!("listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Build the `TokenVerifier` the BFF callback uses to verify the access token
/// the IdP returns (before caching the resolved `AuthUser`). In `dev_noop`
/// there is no IdP, so the noop verifier stands in.
async fn build_token_verifier(config: &AppConfig) -> anyhow::Result<Arc<dyn TokenVerifier>> {
    match config.auth.mode {
        AuthMode::Bff => {
            let issuer_url = config
                .auth
                .issuer_url
                .clone()
                .ok_or_else(|| anyhow::anyhow!("auth.mode=bff requires auth.issuer_url"))?;
            let audience = config
                .auth
                .audience
                .clone()
                .ok_or_else(|| anyhow::anyhow!("auth.mode=bff requires auth.audience"))?;
            let verifier = ZitadelTokenVerifier::new(&ZitadelConfig {
                issuer_url,
                audience,
            })
            .await
            .map_err(|e| anyhow::anyhow!("zitadel verifier init: {e}"))?;
            tracing::info!("auth: Zitadel token verifier ready (BFF callback)");
            Ok(Arc::new(verifier))
        }
        AuthMode::DevNoop => {
            guard_dev_noop()?;
            tracing::warn!("auth: NoopTokenVerifier active — every request becomes the dev user");
            Ok(Arc::new(NoopTokenVerifier::default()))
        }
    }
}

/// Build the per-request authenticator and (in `bff`) the OIDC client. Fails
/// fast on discovery just like `ZitadelTokenVerifier::new`.
async fn build_authenticator(
    config: &AppConfig,
    session_store: Arc<dyn SessionStore>,
) -> anyhow::Result<(Arc<dyn Authenticator>, Option<Arc<OidcClient>>)> {
    match config.auth.mode {
        AuthMode::Bff => {
            let issuer_url = config
                .auth
                .issuer_url
                .clone()
                .ok_or_else(|| anyhow::anyhow!("auth.mode=bff requires auth.issuer_url"))?;
            let client_id = config
                .auth
                .client_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("auth.mode=bff requires auth.client_id"))?;
            // The IdP redirect URI is fixed to the same-origin callback. The
            // SPA proxies /api to the backend in dev and is same-origin in
            // prod, so this is host-relative to the public origin — but the
            // IdP needs an absolute value. Take it from config-derived bootstrap
            // (written by deploy/zitadel/bootstrap.sh) via the post-login
            // origin if present, else assume same-origin localhost dev.
            let redirect_uri = std::env::var("MEKHAN__AUTH__REDIRECT_URI")
                .ok()
                .unwrap_or_else(|| "http://localhost:15173/api/auth/callback".to_string());

            let oidc = OidcClient::discover(OidcConfig {
                issuer_url,
                client_id,
                client_secret: config.auth.client_secret.clone(),
                redirect_uri,
                scopes: config.auth.scopes.clone(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("oidc discovery init: {e}"))?;
            let oidc = Arc::new(oidc);
            tracing::info!("auth: BFF authenticator ready (server-side OIDC)");
            Ok((
                Arc::new(BffAuthenticator::new(session_store, oidc.clone())),
                Some(oidc),
            ))
        }
        AuthMode::DevNoop => {
            guard_dev_noop()?;
            tracing::warn!("auth: NoopAuthenticator active — every request is the dev user");
            Ok((Arc::new(NoopAuthenticator::default()), None))
        }
    }
}

/// Build the optional RFC 7662 introspection verifier for machine PATs.
/// Active only in `bff` mode when an introspection API credential is fully
/// configured; otherwise `None` and the Bearer path stays disabled (cookie
/// auth only — `dev_noop` already lets every request through).
async fn build_introspection(
    config: &AppConfig,
) -> anyhow::Result<Option<Arc<IntrospectionVerifier>>> {
    if config.auth.mode != AuthMode::Bff {
        return Ok(None);
    }
    let (Some(issuer), Some(client_id), Some(client_secret)) = (
        config.auth.issuer_url.as_deref(),
        config.auth.introspection_client_id.clone(),
        config.auth.introspection_client_secret.clone(),
    ) else {
        tracing::info!(
            "auth: introspection disabled (no auth.introspection_client_id/secret) \
             — machine PAT auth unavailable"
        );
        return Ok(None);
    };
    let verifier = IntrospectionVerifier::new(issuer, client_id, client_secret)
        .await
        .map_err(|e| anyhow::anyhow!("introspection init: {e}"))?;
    Ok(Some(Arc::new(verifier)))
}

/// Build the optional Zitadel Management broker for the embedded
/// `/api/v1/auth/tokens` feature. Active only in `bff` mode when `auth.broker_pat`
/// is configured (provisioned by `deploy/zitadel/bootstrap.sh`); otherwise
/// `None` and the token endpoints 503 / the UI hides the section. Mirrors
/// [`build_introspection`]; synchronous — the client validates its PAT lazily.
fn build_zitadel_mgmt(config: &AppConfig) -> anyhow::Result<Option<Arc<ZitadelMgmt>>> {
    if config.auth.mode != AuthMode::Bff {
        return Ok(None);
    }
    let (Some(issuer), Some(broker_pat)) = (
        config.auth.issuer_url.as_deref(),
        config.auth.broker_pat.clone(),
    ) else {
        tracing::info!(
            "auth: token broker disabled (no auth.broker_pat) \
             — embedded /api/v1/auth/tokens unavailable"
        );
        return Ok(None);
    };
    let mgmt = ZitadelMgmt::new(issuer, broker_pat)
        .map_err(|e| anyhow::anyhow!("zitadel mgmt init: {e}"))?;
    tracing::info!("auth: Zitadel token broker ready (embedded PAT management)");
    Ok(Some(Arc::new(mgmt)))
}

/// Refuse to boot a dev-mode credential bypass in production.
fn guard_dev_noop() -> anyhow::Result<()> {
    let prod = std::env::var("MEKHAN_ENV")
        .map(|v| v.eq_ignore_ascii_case("prod") || v.eq_ignore_ascii_case("production"))
        .unwrap_or(false);
    if prod {
        anyhow::bail!("auth.mode=dev_noop is forbidden when MEKHAN_ENV=prod");
    }
    Ok(())
}
