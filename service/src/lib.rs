#![allow(dead_code)]

pub mod analytics;
pub mod auth;
pub mod autoscaler;
pub mod backends;
pub mod bootstrap;
pub mod catalogue;
pub mod causality;
pub mod compiler;

pub mod config;
pub mod data;
pub mod db;
pub mod demos;
pub mod file_servers;
pub mod fleet;
pub mod handlers;
pub mod inventory;
pub mod lifecycle;
/// Legacy-migration pipeline driver (docs/32 Phase 5). Transport-agnostic
/// crawl → reconcile → targeted-hash → register logic, invoking the REAL
/// executor-file-ops crawl/probe ops IN-PROCESS against a Local
/// `StorageConfig`. Feature-gated so the default service build pulls in none of
/// the file-ops / OpenDAL deps. The `legacy-migration-driver` bin is a thin
/// clap wrapper over this module; the integration test calls it directly.
#[cfg(feature = "migration-driver")]
pub mod migration_driver;
pub mod model_serving_group;
pub mod models;
pub mod nats;
pub mod nodes;
pub mod notify;
pub mod observability;
pub mod openapi;
pub mod petri;
pub mod presence;
pub mod process;
pub mod projections;
pub mod query;
pub mod runner_commands;
pub mod runners_nats;
pub mod s3;
pub mod scope;
pub mod streams;
pub mod triggers;
pub mod worker_groups;
pub mod yjs;

use std::sync::Arc;

use std::path::PathBuf;

use axum::{
    extract::DefaultBodyLimit,
    http::{header, HeaderValue, Method},
    routing::get,
    Router,
};
use sqlx::PgPool;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_swagger_ui::SwaggerUi;

use crate::auth::authenticator::Authenticator;
use crate::auth::bff::oidc::OidcClient;
use crate::auth::bff::session::SessionStore;
use crate::auth::{PrincipalResolver, TokenVerifier};
use crate::catalogue::repository::CatalogueRepository;
use crate::causality::live::LiveBroadcasts;
use crate::config::AppConfig;
use crate::nats::MekhanNats;
use crate::openapi::ApiDoc;
use crate::petri::client::PetriClient;
use crate::s3::ArtifactStore;
use crate::triggers::TriggerDispatcher;
use crate::yjs::manager::YjsManager;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub petri: PetriClient,
    pub nats: MekhanNats,
    pub config: AppConfig,
    pub yjs: Arc<YjsManager>,
    pub s3: Arc<ArtifactStore>,
    pub artifact_s3: Option<Arc<ArtifactStore>>,
    pub catalogue_repo: Arc<dyn CatalogueRepository>,
    pub live: Arc<LiveBroadcasts>,
    /// Per-request authn seam (cookie → `AuthUser`). `bff` or `dev_noop`.
    pub authenticator: Arc<dyn Authenticator>,
    /// Server-side session custody (token set + in-flight PKCE flows).
    pub session_store: Arc<dyn SessionStore>,
    /// Server-side OIDC client. `None` in `dev_noop` (no IdP to talk to).
    pub oidc: Option<Arc<OidcClient>>,
    /// JWT verifier — still used, but only internally by the BFF callback to
    /// verify the token the IdP returns before caching the `AuthUser`.
    pub token_verifier: Arc<dyn TokenVerifier>,
    /// Claims → `AuthUser` mapper. Reused unchanged by the BFF callback and
    /// the introspection Bearer path.
    pub principal_resolver: Arc<dyn PrincipalResolver>,
    /// RFC 7662 introspection for machine PATs (CI `mekhan apply`). `None`
    /// unless an introspection API credential is configured — then the
    /// Bearer path in `require_auth_middleware` is disabled.
    pub introspection: Option<Arc<crate::auth::IntrospectionVerifier>>,
    /// Zitadel Management broker for the embedded `/api/v1/auth/tokens` feature
    /// (per-user automation PATs). `None` unless `auth.broker_pat` is
    /// configured — then those endpoints 503 and the SPA hides the section.
    pub zitadel_mgmt: Option<Arc<crate::auth::ZitadelMgmt>>,
    pub triggers: Arc<TriggerDispatcher>,
    /// In-flight WaitForResult waiters, shared with the lifecycle consumer.
    pub result_waiters: Arc<crate::triggers::ResultWaiters>,
    /// Phase B.9 — write-side secret backend used by the Resource CRUD
    /// handlers. `VaultResourceStore` when `VAULT_ADDR`/`VAULT_TOKEN` are
    /// set; `InMemoryResourceStore` (dev/test fallback) otherwise. The
    /// engine's wrap path still reads through the existing `SecretStore`
    /// path — this trait is write-only by design.
    pub resource_store: Arc<dyn aithericon_resources::ResourceSecretStore>,
    /// READ-side secret backend used by the file-server serve bridge
    /// (`data::serve`) to resolve an endpoint's `resource_ref` → the s3/sftp
    /// credential fields stored in Vault (keyed `<vault_path>#<field>`).
    /// `VaultSecretStore` when VAULT_ADDR/VAULT_TOKEN are set; an empty
    /// `InMemorySecretStore` (dev/test) otherwise. Distinct from
    /// `resource_store` (write-only): the engine's wrap path and this serve
    /// read path both go through the same `SecretStore::get` contract.
    pub secret_store: Arc<dyn aithericon_secrets::SecretStore>,
    /// Publish-time resource resolver. Reads workspace resources +
    /// per-version public config, runs ACL + audit, and returns the JSON
    /// envelope the publish handler splices into the AIR before
    /// persistence. The launcher never touches this — instances run against
    /// already-spliced AIR.
    pub resource_resolver: Arc<crate::petri::resource_resolver::ResourceResolver>,
    /// Phase 2 (Lab Runner Fleet) — mints scoped per-runner NATS *user* JWTs
    /// signed by the `runners`-account signing key. Always present: resolved
    /// at startup (auto-generates + persists a stable dev seed on a miss).
    pub runner_nats_signer: Arc<crate::runners_nats::RunnerNatsSigner>,
    /// Phase 5 (Lab Runner Fleet) — shared handle to the presence controller's
    /// in-memory `PresenceMap`. The same map the Phase-3 subscriber/sweep tasks
    /// mutate; `GET /api/v1/runners/presence` reads through it for live pool
    /// capacity (which runners hold an admitted unit right now).
    pub runner_presence: crate::presence::RunnerPresence,
    /// Humans-as-a-capacity (docs/33 §7) — shared handle to the human presence
    /// controller's in-memory map. The same map the subscriber/sweep tasks
    /// mutate; the human analogue of `runner_presence`. A roster MEMBER's
    /// availability (not a daemon heartbeat) drives admission into the
    /// `pool-<capacity_id>` net, reusing the runner pool plumbing verbatim.
    pub human_presence: crate::presence::HumanPresence,
    /// Unified fleet-liveness registry (docs/23 §2; docs/24 S1) — the shared
    /// telemetry plane over BOTH the anonymous worker pool and the advisory
    /// facet of enrolled runners. Workers heartbeat on `worker.*.presence`
    /// (subscriber + TTL sweep owned by `crate::fleet`); runners mirror their
    /// self-reported backends in from `crate::presence::runners` on each
    /// heartbeat. Publish reads through it (`serves_backend`) to WARN (never
    /// fail) on a step's backend served by zero live capacities. Purely
    /// advisory — a dropped capacity NEVER reaps an instance (the runner control
    /// binding in `presence::runners` is a separate plane).
    pub fleet: crate::fleet::FleetLiveness,
    /// Publish-time asset resolver (docs/20 §5). Materializes the pinned
    /// records of every node-bound asset into the JSON envelope the publish
    /// handler splices into the AIR (`__assets`) before persistence. The
    /// launcher never touches this — instances run against already-spliced AIR,
    /// symmetric with `resource_resolver`.
    pub asset_resolver: Arc<crate::petri::asset_resolver::AssetResolver>,
    /// Transactional-email delivery. The hexagonal `Mailer` port — SMTP / Brevo
    /// / log adapter chosen from `EmailConfig` (log by default, offline).
    pub email: Arc<dyn crate::notify::email::Mailer>,
    /// Invite-accept identity provisioner (Phase 4). `None` only when a real
    /// auth mode lacks broker credentials → accept 503s. Under `dev_noop` it's
    /// the deterministic Noop. Boot-checked against `auth.mode`.
    pub user_provisioner: Option<Arc<dyn crate::auth::provisioner::UserProvisioner>>,
}

/// Public OpenApiRouter — routes mounted OUTSIDE the auth gate.
///
/// Currently only the `/healthz` liveness probe. Anything that load balancers,
/// uptime monitors, or container orchestrators need to reach without a session
/// cookie belongs here.
fn build_public_openapi_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::new()
        .routes(routes!(handlers::health::liveness))
        // Runner enrollment — PUBLIC by design: authed by the `rt_`
        // registration token in the body, not the session cookie, so a fresh
        // runner can bootstrap its `rnr_` credential before it has one.
        .routes(routes!(handlers::runners::enroll_runner))
        // Worker enrollment — PUBLIC by the same design as runner enroll: authed
        // by the `wt_` registration token in the body, so a fresh worker can
        // bootstrap its `wkr_` credential + scoped JWT before it has any.
        .routes(routes!(handlers::workers::enroll_worker))
        // Model-serving inventory — PUBLIC by design (docs/29 GAP A): the
        // inference router is an in-cluster control-plane peer with no session
        // cookie, and this returns only the in-cluster runner base_urls/model_ids
        // the router already holds to route. No credential, no workspace leak.
        .routes(routes!(handlers::model_pool::list_model_serving_runners))
        // Invite preview + accept — PUBLIC by design (Phase 4 auth-bootstrap
        // exception, like /api/auth/*): an invitee has no session yet. Authed by
        // the opaque token in the path, not the session cookie. Registered here
        // (not a raw merged router) so the typed wrappers land in the OpenAPI
        // spec and openapi-drift catches changes.
        .routes(routes!(handlers::invites::preview_invite))
        .routes(routes!(handlers::invites::accept_invite))
}

/// Protected OpenApiRouter — every `#[utoipa::path]`-annotated handler that
/// requires authentication. Mounted behind `require_auth_middleware`.
fn build_protected_openapi_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::<AppState>::with_openapi(ApiDoc::openapi())
        // Backend registry — drives the editor's AutomatedStep panel
        // (picker labels/icons, default config seed, default output port).
        // `derive-output` is the config→Port hook for `Derived` backends
        // (LLM today); the editor calls it on every step config change so
        // the read-only port section reflects the actual runtime envelope.
        .routes(routes!(handlers::backends::list_backends))
        .routes(routes!(handlers::backends::derive_backend_output))
        // Node-type registry — drives the editor's palette + property-panel
        // dispatch. Companion to `/api/v1/backends`; the Svelte component
        // map and Lucide icons stay frontend-only.
        .routes(routes!(handlers::node_types::list_node_types))
        // Library-node catalogue — the "Library" half of the editor palette
        // (branded, reusable sub-workflow building blocks). Data-driven and
        // ACL-filtered, unlike the static node-type registry above.
        .routes(routes!(handlers::node_library::list_node_library))
        .routes(routes!(handlers::node_library::list_library_categories))
        // Library packs — named, importable/exportable bundles of library
        // nodes. `import` + `export` are registered BEFORE the `{id}` routes so
        // the literal segments win against the `{id}` wildcard in the trie (same
        // reasoning as the templates `apply-air` ordering note below).
        .routes(routes!(handlers::library_packs::list_packs))
        .routes(routes!(handlers::library_packs::import_pack))
        .routes(routes!(handlers::library_packs::export_pack))
        .routes(routes!(handlers::library_packs::get_pack))
        .routes(routes!(handlers::library_packs::delete_pack))
        // Custom uploaded library logos — lightweight image blob store keyed by
        // an opaque id (NOT the asset-type/record system). `presentation.icon`
        // carries an `asset:{id}` token that the frontend resolves via the GET.
        .routes(routes!(handlers::library_packs::upload_library_icon))
        .routes(routes!(handlers::library_packs::get_library_icon))
        // Auth tokens — embedded per-user PAT management. Cookie-only by
        // construction (the `AuthUser` arg re-runs the cookie authenticator,
        // so a Bearer PAT behind `require_auth_middleware` can't reach these
        // and thus can't mint more tokens).
        .routes(routes!(
            handlers::auth_tokens::list_tokens,
            handlers::auth_tokens::create_token
        ))
        .routes(routes!(handlers::auth_tokens::revoke_token))
        // Templates — `apply_air_template` (POST /api/templates/apply-air)
        // MUST be registered BEFORE the `{id}` routes; matchit/axum match
        // literal segments only when they're seen first against a wildcard
        // already in the trie at the same position. Otherwise `apply-air`
        // gets routed to `GET/PUT/DELETE /api/templates/{id}` (with
        // `id = "apply-air"`) and POST returns 405 (#126.4.1 cert finding).
        .routes(routes!(handlers::templates::apply_air_template))
        .routes(routes!(
            handlers::templates::list_templates,
            handlers::templates::create_template
        ))
        .routes(routes!(
            handlers::templates::get_template,
            handlers::templates::update_template,
            handlers::templates::delete_template
        ))
        .routes(routes!(handlers::templates::get_template_bundle))
        .routes(routes!(handlers::templates::get_io_contract))
        .routes(routes!(handlers::templates::publish_template))
        .routes(routes!(handlers::templates::new_version))
        .routes(routes!(handlers::governance::promote_template))
        .routes(routes!(handlers::governance::demote_template))
        .routes(routes!(handlers::governance::fork_library_node))
        .routes(routes!(handlers::fork::fork_template))
        .routes(routes!(handlers::governance::set_lifecycle))
        .routes(routes!(handlers::governance::library_upgrade_preview))
        .routes(routes!(handlers::templates::discard_draft))
        .routes(routes!(handlers::templates::apply_template))
        .routes(routes!(handlers::templates::list_versions))
        .routes(routes!(handlers::templates::get_latest))
        .routes(routes!(handlers::templates::get_air))
        .routes(routes!(handlers::templates::compile_preview))
        .routes(routes!(handlers::templates::io_stubs))
        .routes(routes!(handlers::templates::compile_graph))
        .routes(routes!(handlers::templates::analyze_graph))
        // Per-template usage analytics (rollup-backed summary + timeseries)
        .routes(routes!(analytics::template::template_analytics))
        .routes(routes!(analytics::template::template_timeseries))
        // Admin: remove / reseed built-in demos
        .routes(routes!(handlers::demos::reset_demos))
        .routes(routes!(handlers::demos::reseed_demos))
        // Cluster/watcher management — read-through of the engine's
        // multi-cluster `ClusterRegistry` (docs/16 §9): list live clusters +
        // force-reconnect / drain a cluster.
        .routes(routes!(handlers::clusters::list_clusters))
        .routes(routes!(handlers::clusters::reconnect_cluster))
        .routes(routes!(handlers::clusters::drain_cluster))
        .routes(routes!(handlers::clusters::list_cluster_leases))
        .routes(routes!(handlers::clusters::fleet_metrics))
        .routes(routes!(handlers::clusters::cluster_metrics))
        // Capacity aggregator — the unified Control-Plane read: every
        // `capacity` + `datacenter` resource classified by the SINGLE dispatch
        // authority (`CapacityAxes::backend`) with live utilization.
        .routes(routes!(handlers::capacities::list_capacities))
        // Model-pool (docs/28 + docs/29 P1) — loaded-set projection + the
        // operator state-machine step. Projection/control seam only: inference
        // bypasses the engine net + presence net, no NATS subjects added.
        .routes(routes!(
            handlers::model_pool::list_loaded_models,
            handlers::model_pool::create_model
        ))
        .routes(routes!(
            handlers::model_pool::get_model,
            handlers::model_pool::delete_model
        ))
        .routes(routes!(handlers::model_pool::transition_model))
        .routes(routes!(handlers::model_pool::load_model))
        .routes(routes!(handlers::model_pool::unload_model))
        // Folded-in autoscale policy (set/clear on the model SET row). The policy
        // used to be its own `model_policy` resource; it now lives on `model_states`.
        .routes(routes!(
            handlers::model_pool::set_model_policy,
            handlers::model_pool::clear_model_policy
        ))
        // Model-pool reconciliation (docs/31 Phase 0) — the per-node engine
        // inventory read model both autoscaler loops + the router consume:
        // base engines, per-engine C, loaded LoRA adapters, headroom.
        .routes(routes!(handlers::fleet_engines::list_fleet_engines))
        // Model-pool P4 (docs/29 §6') — replica-autoscaler Control-Plane read +
        // manual scale. The autoscaler loop reconciles `model_replicas` rows;
        // these surface them + the L1 manual desired override.
        .routes(routes!(
            handlers::model_replicas::list_model_replicas,
            handlers::model_replicas::scale_model_replica
        ))
        .routes(routes!(handlers::model_replicas::get_model_replica))
        // Operator load/unload action — publishes a ModelCommand to a runner's
        // model agent (vLLM admin / Ollama Metal runtime). Control plane only.
        .routes(routes!(
            handlers::model_commands::publish_runner_model_command
        ))
        // Official model-catalog browse (the operator's model browser): scrapes
        // ollama.com / calls the HF JSON API. Metadata only, cached ~10 min.
        .routes(routes!(handlers::model_catalog::browse_model_catalog))
        // Model-pool P5 (docs/29 §7') — inference metering audit-ledger read.
        .routes(routes!(
            handlers::inference_metering::list_inference_requests
        ))
        // Inference telemetry — live router /metrics proxy (point-in-time gauges)
        // + historical per-model timeseries over the durable ledger (TimescaleDB).
        .routes(routes!(handlers::inference_telemetry::router_live_metrics))
        .routes(routes!(handlers::inference_telemetry::inference_timeseries))
        // Template tests
        .routes(routes!(
            handlers::template_tests::list_tests,
            handlers::template_tests::create_test
        ))
        .routes(routes!(
            handlers::template_tests::update_test,
            handlers::template_tests::delete_test
        ))
        .routes(routes!(handlers::template_tests::run_one))
        .routes(routes!(handlers::template_tests::run_all))
        .routes(routes!(handlers::template_tests::list_runs))
        .routes(routes!(handlers::template_tests::promote_instance_to_test))
        // Instances
        .routes(routes!(
            handlers::instances::list_instances,
            handlers::instances::create_instance
        ))
        .routes(routes!(
            handlers::instances::get_instance,
            handlers::instances::cancel_instance
        ))
        .routes(routes!(handlers::instances::get_instance_state))
        .routes(routes!(handlers::instances::get_instance_events))
        .routes(routes!(handlers::instances::list_step_executions))
        .routes(routes!(handlers::instances::list_instance_children))
        .routes(routes!(handlers::instances::list_instance_allocations))
        .routes(routes!(handlers::instances::stream_instance))
        // Executions — data-plane channel byte tap. Generic byte pipe over the
        // executor's `EXECUTOR_DATASTREAM` JetStream stream
        // (`executor.datastream.{execution_id}.{channel}`); ephemeral consumer,
        // streams payload bytes chunk-by-chunk until the EOF envelope.
        .routes(routes!(handlers::executions::tap_channel_data))
        // Executions — subscribe-only LiveKit viewer token. Mints a JWT for the
        // room (`lk_{execution_id}__{channel}`) the executor publishes annotated
        // video frames to, so the browser can join and watch.
        .routes(routes!(handlers::executions::livekit_viewer_token))
        // Streams — workflow-as-streaming-endpoint (docs/25 §9 Phase 3).
        // Ingress: mekhan acts as the VIRTUAL PRODUCER for a stream_source
        // node (deterministic execution id `st-{instance}-{node}`, publishes
        // on the same EXECUTOR_DATASTREAM / EXECUTOR_EVENTS surfaces a real
        // executor job would). Egress: tap a stream_sink's sunk bytes via the
        // descriptor parked in the step_executions projection.
        .routes(routes!(handlers::streams::push_stream_source_data))
        .routes(routes!(handlers::streams::emit_stream_source_items))
        .routes(routes!(handlers::streams::tap_stream_sink_data))
        // Processes (HPI inspection)
        .routes(routes!(process::handlers::list_processes))
        .routes(routes!(process::handlers::process_stats))
        .routes(routes!(
            process::handlers::get_process,
            process::handlers::update_process
        ))
        .routes(routes!(process::handlers::get_process_metrics))
        .routes(routes!(process::handlers::get_process_metrics_summary))
        .routes(routes!(process::handlers::get_process_logs))
        .routes(routes!(process::handlers::get_process_tasks))
        .routes(routes!(process::handlers::get_process_artifacts))
        // Processes-live (SSE)
        .routes(routes!(handlers::process_live::metrics_series))
        .routes(routes!(handlers::process_live::metrics_stream))
        .routes(routes!(handlers::process_live::logs_tail))
        .routes(routes!(handlers::process_live::logs_stream))
        .routes(routes!(handlers::process_live::artifacts_list))
        .routes(routes!(handlers::process_live::artifacts_stream))
        // Tasks
        .routes(routes!(process::handlers::list_tasks))
        // `/tasks/inbox` is a literal child of `/tasks` and MUST be registered
        // before the `/tasks/{id}` wildcard so matchit prefers it.
        .routes(routes!(process::handlers::inbox))
        .routes(routes!(handlers::task_stream::task_stream))
        .routes(routes!(process::handlers::get_task))
        .routes(routes!(process::handlers::complete_task))
        .routes(routes!(process::handlers::cancel_task))
        .routes(routes!(process::handlers::claim_task))
        // Catalogue. Literal children (`facets`, `query-fields`,
        // `saved-queries`) are registered before the
        // `/{execution_id}/{id}` wildcard so matchit prefers them.
        .routes(routes!(catalogue::handlers::list_entries))
        .routes(routes!(catalogue::handlers::stats))
        .routes(routes!(catalogue::handlers::stats_by_net))
        .routes(routes!(catalogue::handlers::lineage))
        .routes(routes!(catalogue::handlers::distinct_values))
        .routes(routes!(catalogue::handlers::distinct_jsonb_values))
        .routes(routes!(catalogue::handlers::facets))
        .routes(routes!(catalogue::handlers::query_fields))
        .routes(routes!(
            catalogue::saved_queries::list_saved_queries,
            catalogue::saved_queries::create_saved_query
        ))
        .routes(routes!(
            catalogue::saved_queries::update_saved_query,
            catalogue::saved_queries::delete_saved_query
        ))
        // `data-types` is another literal child — keep it before the
        // `/{execution_id}/{id}` wildcard below.
        .routes(routes!(
            catalogue::data_types::list_data_types,
            catalogue::data_types::promote_data_type
        ))
        .routes(routes!(
            catalogue::data_types::get_data_type,
            catalogue::data_types::update_data_type,
            catalogue::data_types::delete_data_type
        ))
        .routes(routes!(catalogue::handlers::download_artifact))
        .routes(routes!(catalogue::handlers::get_entry))
        // Inventory (docs/32) — by-reference physical-copy registry. `register`
        // + `stats` are literal segments registered before the list route so
        // matchit prefers them (no `{id}` collision here, but keep the
        // convention). Content-addressed to the catalogue via `content_hash`.
        .routes(routes!(inventory::handlers::register))
        .routes(routes!(inventory::handlers::index))
        .routes(routes!(inventory::handlers::stats))
        .routes(routes!(inventory::handlers::list_entries))
        // Reconcile (docs/32 §4/§5) — classify crawl-observed copies against the
        // legacy baseline; canonical-pick; orphan/duplicate reports.
        .routes(routes!(inventory::handlers::reconcile_batch))
        .routes(routes!(inventory::handlers::mark_canonical))
        .routes(routes!(inventory::handlers::reconcile_summary))
        .routes(routes!(inventory::handlers::reconcile_orphans))
        .routes(routes!(inventory::handlers::reconcile_duplicates))
        // File servers (docs/32 §4.1) — first-class storage backends the
        // platform tracks files on. Hybrid entity: identity + rollups here,
        // secrets in the referenced workspace `resource`.
        .routes(routes!(
            file_servers::handlers::list,
            file_servers::handlers::create
        ))
        .routes(routes!(file_servers::handlers::adopt))
        .routes(routes!(
            file_servers::handlers::get,
            file_servers::handlers::update,
            file_servers::handlers::delete
        ))
        // N access-method endpoints per server (object_store|s3|sftp|local_mount).
        .routes(routes!(
            file_servers::handlers::list_endpoints,
            file_servers::handlers::create_endpoint
        ))
        .routes(routes!(
            file_servers::handlers::update_endpoint,
            file_servers::handlers::delete_endpoint
        ))
        // On-demand hash-probe reconcile of one endpoint (docs/32 §4 Phase 4).
        .routes(routes!(file_servers::handlers::verify_endpoint))
        // Unified Data browser read-model — catalogued entries + nested physical
        // copies (server names resolved) + uncatalogued peek.
        .routes(routes!(data::handlers::entries))
        // Serve bridge — stream an entry's bytes by resolving it to a physical
        // copy + endpoint (local_mount NATS relay / s3 presign-or-proxy / sftp).
        .routes(routes!(data::handlers::entry_content))
        // File analytics (docs/32 Cuts 1+2) — group-by breakdowns over the
        // promoted file_inventory columns (the `directory` dimension doubles
        // as the capacity-treemap level loader) + growth timeseries over
        // `inventory_snapshots` + the manual snapshot trigger.
        .routes(routes!(analytics::handlers::breakdown))
        .routes(routes!(analytics::handlers::timeseries))
        .routes(routes!(analytics::handlers::snapshot))
        // Provenance
        .routes(routes!(causality::routes::token_provenance))
        .routes(routes!(causality::routes::cross_link))
        .routes(routes!(causality::routes::provenance_from_artifact))
        .routes(routes!(causality::routes::event_detail))
        // Files (upload has a 50 MB body limit applied at the merged-router level
        // since utoipa-axum doesn't expose per-route layers here)
        .routes(routes!(handlers::files::upload_file))
        .routes(routes!(handlers::files::get_file))
        // Resources (Phase B.9) — typed credential CRUD. Read paths hit
        // the DB only; write paths (POST/PUT/rotate/delete) also touch the
        // configured `ResourceSecretStore` (Vault in prod, in-memory in
        // dev/test). The `/types` introspection route powers the picker.
        .routes(routes!(
            handlers::resources::list_resources,
            handlers::resources::create_resource
        ))
        .routes(routes!(handlers::resources::list_resource_types))
        .routes(routes!(
            handlers::resources::get_resource,
            handlers::resources::update_resource,
            handlers::resources::delete_resource
        ))
        .routes(routes!(handlers::resources::rotate_resource))
        .routes(routes!(handlers::resources::move_resource))
        .routes(routes!(handlers::resources::list_resource_audit))
        // Runners (Phase 1, Lab Runner Fleet) — workspace-scoped runner fleet
        // + GitLab-style enrollment. `enroll` is mounted on the PUBLIC router
        // (authed by the `rt_` token in the body); everything here is behind
        // the auth gate. The literal `registration-tokens` routes are
        // registered BEFORE `{id}` so matchit prefers the literal segment over
        // the `{id}` wildcard (same trie-ordering caveat as templates'
        // `apply-air`). Heartbeat is runner-token authed (the `rnr_` bearer
        // resolves to a `runner:{id}` principal via require_auth_middleware).
        .routes(routes!(
            handlers::runners::create_registration_token,
            handlers::runners::list_registration_tokens
        ))
        .routes(routes!(handlers::runners::revoke_registration_token))
        // Phase 5 — live in-memory presence snapshot. Registered BEFORE the
        // `{id}` routes so matchit prefers the literal `presence` segment over
        // the `{id}` wildcard (same trie-ordering caveat as `registration-tokens`).
        .routes(routes!(handlers::runners::runner_presence))
        .routes(routes!(handlers::runners::list_runners))
        .routes(routes!(handlers::runners::heartbeat_runner))
        // Worker-pool coverage (worker pool — anonymous competing consumers, NOT
        // enrolled runners). Live worker presence + per-backend coverage.
        .routes(routes!(handlers::workers::worker_coverage))
        // Workers (Phase A, Grouped + Enrolled Workers) — the identity plane on
        // the executor worker pool: enrolled, group-scoped, revocable workers
        // that still PULL. `enroll` is on the PUBLIC router (authed by the `wt_`
        // token in the body); everything here is behind the auth gate. The
        // literal `registration-tokens` + `coverage` segments are registered
        // BEFORE `{id}` so matchit prefers the literal over the `{id}` wildcard
        // (same trie-ordering caveat as the runner block). Heartbeat + nats-creds
        // are worker-token authed, self-only (subject == worker:{id}).
        .routes(routes!(
            handlers::workers::create_worker_registration_token,
            handlers::workers::list_worker_registration_tokens
        ))
        .routes(routes!(handlers::workers::revoke_worker_registration_token))
        .routes(routes!(handlers::workers::list_workers))
        .routes(routes!(handlers::workers::heartbeat_worker))
        .routes(routes!(handlers::workers::issue_worker_nats_creds))
        .routes(routes!(
            handlers::workers::get_worker,
            handlers::workers::revoke_worker
        ))
        // Phase 2 — self-service NATS scoped-creds mint/rotation. Runner-token
        // authed, self-only (subject == runner:{id}), same boundary as
        // heartbeat. Mints a fresh user JWT from the stored nats_public_key.
        .routes(routes!(handlers::runners::issue_runner_nats_creds))
        // Phase 3 — runner interface catalog (ROS topics/services/actions).
        // POST is runner-token authed, self-only (subject == runner:{id}), same
        // boundary as heartbeat; GET is session/human authed + workspace-scoped
        // (same boundary as get_runner). The `{id}/interfaces` literal segment is
        // registered alongside the other `{id}/...` routes.
        .routes(routes!(
            handlers::runners::upsert_runner_interfaces,
            handlers::runners::get_runner_interfaces
        ))
        .routes(routes!(
            handlers::runners::get_runner,
            handlers::runners::revoke_runner
        ))
        // Roster (docs/33 §7 — humans as a capacity). The human counterpart to
        // the runner fleet: the set of `workspace_members` enrolled into a human
        // `capacity` resource backed by a `pool-<capacity_id>` net. The literal
        // `/roster/me` + `/roster/availability` segments are registered BEFORE
        // `/roster/{id}` so matchit prefers the literal over the `{id}` wildcard
        // (same trie-ordering caveat as the runner block).
        .routes(routes!(
            handlers::roster::enroll_member,
            handlers::roster::list_roster
        ))
        .routes(routes!(handlers::roster::my_enrollments))
        .routes(routes!(handlers::roster::set_availability))
        .routes(routes!(handlers::roster::human_presence))
        .routes(routes!(handlers::roster::human_presence_heartbeat))
        .routes(routes!(
            handlers::roster::get_roster_member,
            handlers::roster::update_roster_member,
            handlers::roster::revoke_roster_member
        ))
        // Admin engine-net overview + kill-switch / cleanup (2026-06-10
        // incident follow-up). Engine-wide list with per-net event counts,
        // DELETE = engine terminate-with-cleanup, purge-events = PETRI_GLOBAL
        // subject purge for terminal nets. All require workspace Admin.
        .routes(routes!(handlers::admin_nets::list_admin_nets))
        .routes(routes!(handlers::admin_nets::bulk_kill_nets))
        .routes(routes!(handlers::admin_nets::purge_terminal_nets))
        .routes(routes!(handlers::admin_nets::kill_admin_net))
        .routes(routes!(handlers::admin_nets::purge_admin_net_events))
        // Capability types (Phase 4 — typed capability registry). Admin-curated,
        // workspace-scoped vocabulary the enroll path validates runner caps
        // against and the publish path validates step Requirements against.
        // List/create on the collection, get/revoke on `{id}`. Create + revoke
        // are cookie-only (browser admin boundary, same as registration-token
        // mint) so a machine token can't curate the vocabulary.
        .routes(routes!(
            handlers::capabilities::list_capability_types,
            handlers::capabilities::create_capability_type
        ))
        .routes(routes!(
            handlers::capabilities::get_capability_type,
            handlers::capabilities::delete_capability_type
        ))
        // Assets (docs/20) — user-typed, curated static content. Asset TYPES
        // are user-defined schemas (`Vec<PortField>`, additive-only evolution);
        // ASSETS are version-pinned scope-owned collections of schema-validated
        // JSONB records (+ S3 for File fields). Scope-resolved list endpoints
        // (most-specific-wins, docs/20 §2). No Vault — record data is plain.
        .routes(routes!(
            handlers::assets::list_asset_types,
            handlers::assets::create_asset_type
        ))
        .routes(routes!(
            handlers::assets::get_asset_type,
            handlers::assets::update_asset_type,
            handlers::assets::delete_asset_type
        ))
        .routes(routes!(handlers::assets::move_asset_type))
        .routes(routes!(
            handlers::assets::list_assets,
            handlers::assets::create_asset
        ))
        .routes(routes!(
            handlers::assets::get_asset,
            handlers::assets::delete_asset
        ))
        .routes(routes!(handlers::assets::move_asset))
        .routes(routes!(handlers::assets::put_asset_records))
        .routes(routes!(handlers::assets::import_asset_csv))
        .routes(routes!(handlers::assets::upload_asset_file))
        .routes(routes!(handlers::assets::asset_usage))
        // Job templates (Phase 3, B-model) — versioned cluster job-spec entity
        // (flavor-tagged slurm/nomad) + staging join. Mirrors the resources
        // CRUD + versioning pattern but with NO Vault coupling. DB-only.
        .routes(routes!(
            handlers::job_templates::list_job_templates,
            handlers::job_templates::create_job_template
        ))
        .routes(routes!(
            handlers::job_templates::get_job_template,
            handlers::job_templates::update_job_template,
            handlers::job_templates::delete_job_template
        ))
        .routes(routes!(handlers::job_templates::list_job_template_stagings))
        // Stage a template version onto datacenter(s) (Phase 4, B-staging) —
        // kicks a generated staging Petri-net per (version × datacenter).
        // Registered AFTER {id}/stagings so matchit prefers the literal path.
        .routes(routes!(handlers::job_templates::stage_job_template))
        // Materialize a container-image version onto datacenter(s) (container
        // staging) — kicks a generated one-shot materialize Petri-net per
        // (version × datacenter). Explicit targets only.
        .routes(routes!(
            handlers::container_images::materialize_container_image
        ))
        // Triggers (Phase 5)
        .routes(routes!(handlers::triggers::list_triggers))
        .routes(routes!(handlers::triggers::list_template_triggers))
        .routes(routes!(handlers::triggers::fire_trigger))
        .routes(routes!(handlers::triggers::fire_trigger_sync))
        .routes(routes!(handlers::triggers::set_trigger_enabled))
        .routes(routes!(handlers::triggers::trigger_history))
        .routes(routes!(handlers::triggers::preview_cron))
        .routes(routes!(handlers::triggers::trigger_metrics))
        .routes(routes!(handlers::triggers::trigger_source_scope))
        .routes(routes!(handlers::observability::list_silent_drops))
        // Workspaces (Phase A2) — membership-keyed tenant boundary.
        // Self-serve `create_workspace` (caller becomes owner); the rest
        // manage *members* of existing workspaces.
        .routes(routes!(
            handlers::workspaces::list_workspaces,
            handlers::workspaces::create_workspace
        ))
        .routes(routes!(
            handlers::workspaces::get_workspace,
            handlers::workspaces::delete_workspace
        ))
        .routes(routes!(
            handlers::workspaces::list_members,
            handlers::workspaces::add_member
        ))
        .routes(routes!(handlers::workspaces::remove_member))
        .routes(routes!(handlers::workspaces::update_member_role))
        // Invites (Phase 4) — Admin-gated create/list, plus per-invite resend/
        // revoke. The PUBLIC preview/accept endpoints are registered in
        // build_public_openapi_router (auth-bootstrap exception).
        .routes(routes!(
            handlers::invites::create_invite,
            handlers::invites::list_invites
        ))
        .routes(routes!(handlers::invites::resend_invite))
        .routes(routes!(handlers::invites::revoke_invite))
        // Object grants (Phase 3) — per-object ACLs for folders / templates /
        // instances. GET = effective access list; PUT/DELETE edit direct grants.
        .routes(routes!(handlers::object_grants::list_folder_grants))
        .routes(routes!(
            handlers::object_grants::put_folder_grant,
            handlers::object_grants::delete_folder_grant
        ))
        .routes(routes!(handlers::object_grants::list_template_grants))
        .routes(routes!(
            handlers::object_grants::put_template_grant,
            handlers::object_grants::delete_template_grant
        ))
        .routes(routes!(handlers::object_grants::list_instance_grants))
        .routes(routes!(
            handlers::object_grants::put_instance_grant,
            handlers::object_grants::delete_instance_grant
        ))
        .routes(routes!(handlers::object_grants::list_resource_grants))
        .routes(routes!(
            handlers::object_grants::put_resource_grant,
            handlers::object_grants::delete_resource_grant
        ))
        .routes(routes!(handlers::object_grants::list_asset_grants))
        .routes(routes!(
            handlers::object_grants::put_asset_grant,
            handlers::object_grants::delete_asset_grant
        ))
        // Folders — single-parent hierarchical grouping of templates within
        // a workspace (filesystem model). Not an ACL boundary.
        .routes(routes!(handlers::folders::list_workspace_tags))
        .routes(routes!(
            handlers::folders::list_folders,
            handlers::folders::create_folder
        ))
        .routes(routes!(
            handlers::folders::delete_folder,
            handlers::folders::update_folder
        ))
        .routes(routes!(
            handlers::folders::set_template_folder,
            handlers::folders::get_template_folder
        ))
        // Template tags + visibility (tags are a separate cross-cutting system).
        .routes(routes!(
            handlers::folders::get_template_tags,
            handlers::folders::set_template_tags
        ))
        .routes(routes!(handlers::folders::set_template_visibility))
        .routes(routes!(handlers::fork::fork_folder))
        // Per-folder OpenAPI bundle — synthesized trigger spec for SDK
        // generators + API doc viewers, gathered across the folder subtree.
        .routes(routes!(handlers::openapi_bundle::folder_openapi_bundle))
        // Pages — free-form collaborative rich-text docs (Edra + Yjs). Either
        // ride a host 1:1 (template/instance singleton tab) or live free in a
        // folder. Permissions inherit from the host (no per-page ACL); the
        // rich content rides the generalized Yjs stack (doc_kind = 'page').
        .routes(routes!(handlers::pages::create_page))
        .routes(routes!(handlers::pages::upsert_attached_page))
        .routes(routes!(
            handlers::pages::get_page,
            handlers::pages::update_page,
            handlers::pages::delete_page
        ))
        .routes(routes!(handlers::pages::list_folder_pages))
        .routes(routes!(handlers::pages::get_template_page))
        .routes(routes!(handlers::pages::get_instance_page))
        // Active-workspace switcher (Phase B) — per-session override cookie.
        .routes(routes!(
            handlers::me::set_active_workspace,
            handlers::me::clear_active_workspace
        ))
        // Dev-only identity switcher — impersonate a seeded dev user under
        // `dev_noop` (empty roster / 404 in any real auth mode).
        .routes(routes!(handlers::dev_identity::list_dev_identities))
        .routes(routes!(handlers::dev_identity::set_dev_identity))
        // Email → OIDC subject resolver (Phase B) — for the member-admin UI.
        .routes(routes!(handlers::users::resolve_user_by_email))
        // Batch UUID → profile resolver (identity seam) — the SPA's UserChip
        // profile cache coalesces scattered authorship/grant UUIDs through this.
        .routes(routes!(handlers::users::resolve_profiles))
}

/// Collect both OpenApiRouter halves on a thread with explicit stack headroom.
///
/// utoipa's derived schema collection materializes each DTO's full schema tree
/// through by-value builder temporaries in ONE monolithic frame per type — for
/// the big template DTOs (`WorkflowNodeData`, reached via
/// `CreateTemplateRequest` in `routes!()` auto-discovery) the debug-build
/// frames sum to ~2 MiB across a <50-frame chain. That sits exactly at the
/// default stack size of libtest threads and tokio workers, so any codegen
/// layout shift (a module split, a new enum variant) can tip a 2 MiB caller
/// into a stack overflow. 16 MiB of headroom makes router/spec construction
/// stack-safe from any calling context.
#[allow(clippy::type_complexity)]
fn collect_openapi_parts() -> (
    (Router<AppState>, utoipa::openapi::OpenApi),
    (Router<AppState>, utoipa::openapi::OpenApi),
) {
    std::thread::Builder::new()
        .name("openapi-collect".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            (
                build_protected_openapi_router().split_for_parts(),
                build_public_openapi_router().split_for_parts(),
            )
        })
        .expect("failed to spawn openapi-collect thread")
        .join()
        .expect("openapi collection panicked")
}

pub fn build_router(state: AppState) -> Router {
    let frontend_dir = state.config.frontend_dir.clone();
    let cors_config = state.config.clone();

    // Versioning policy:
    //   - JSON API surface lives under `/api/v1/*` (path attrs embed the
    //     version directly).
    //   - `/healthz` sits at the root, outside auth, k8s-conventional.
    //   - The unversioned siblings (`/api/auth/*` OAuth bootstrap,
    //     `/api/yjs/{template_id}` WS,
    //     `/api/triggers/webhook/{workspace_id}/{slug}`) are
    //     NOT OpenAPI-modeled and have external contracts mekhan does not
    //     control.
    //
    // The protected OpenApiRouter holds every authenticated handler; the
    // public one holds only `/healthz`. Both contribute to the same
    // `api_spec` so the published OpenAPI document stays a single document.
    let ((protected_router, mut api_spec), (public_router, public_spec)) = collect_openapi_parts();
    api_spec.merge(public_spec);

    // The auth middleware gates every JSON API route. The WS endpoint is
    // mounted OUTSIDE this layer because it isn't OpenAPI-modeled — it
    // authenticates inside the handler via the same `mekhan_session` cookie
    // (which rides the same-origin WS upgrade) through the `Authenticator`.
    let protected: Router = protected_router
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::extractor::require_auth_middleware,
        ))
        .with_state(state.clone());

    let public: Router = public_router.with_state(state.clone());

    let ws_router: Router = Router::new()
        // The `page` literal out-specifies the `{template_id}` capture in
        // matchit, so `/api/yjs/page/{id}` routes to the page handler and every
        // other `/api/yjs/{uuid}` falls through to the graph handler — no
        // shadowing. Both are unmodeled binary WS (no OpenAPI).
        .route(
            "/api/yjs/page/{page_id}",
            get(handlers::yjs_sync::page_ws_handler),
        )
        .route(
            "/api/yjs/{template_id}",
            get(handlers::yjs_sync::ws_handler),
        )
        .with_state(state.clone());

    // Cloud-layer visualization proxy: mounted INSIDE the auth middleware
    // (joined via merge after the protected router is built). Routes are not
    // OpenAPI-modelled — they're BFF pass-throughs, not first-party resources.
    let cloud_layer_router: Router = Router::new()
        .route(
            "/api/cloud-layer/runs/{run_id}/topology",
            get(handlers::cloud_layer_proxy::get_topology),
        )
        .route(
            "/api/cloud-layer/runs/{run_id}/stream",
            get(handlers::cloud_layer_proxy::get_stream),
        )
        .route(
            "/api/cloud-layer/runs/{run_id}/tokens/{token_id}/payload",
            get(handlers::cloud_layer_proxy::get_token_payload),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::extractor::require_auth_middleware,
        ))
        .with_state(state.clone());

    // BFF auth endpoints — UNAUTHENTICATED (they establish the very session
    // the protected router requires). Same `/api/auth/*` prefix so the Vite
    // dev proxy and prod same-origin SPA serving work with no new rules.
    let auth_router: Router = Router::new()
        .route("/api/auth/login", get(auth::bff::handlers::login))
        .route("/api/auth/callback", get(auth::bff::handlers::callback))
        .route("/api/auth/session", get(auth::bff::handlers::session))
        .route(
            "/api/auth/logout",
            axum::routing::post(auth::bff::handlers::logout),
        )
        .with_state(state.clone());

    // Webhook receivers (Phase 5e): public, unauth'd — auth is performed
    // inside the handler based on the trigger's `WebhookAuth` policy.
    let webhook_router: Router = Router::new()
        .route(
            "/api/triggers/webhook/{workspace_id}/{slug}",
            axum::routing::post(handlers::triggers::webhook_receiver)
                .get(handlers::triggers::webhook_receiver)
                .put(handlers::triggers::webhook_receiver)
                .patch(handlers::triggers::webhook_receiver)
                .delete(handlers::triggers::webhook_receiver),
        )
        .with_state(state.clone());

    // Engine reverse proxy: `/petri/*` → `config.petri_lab_url`. Gives the
    // SPA a single-origin posture in prod (no separate engine ingress) and
    // closes the dev/prod parity gap the Vite proxy used to paper over.
    // Inside `protected` so session-cookie auth gates engine access too.
    let petri_proxy = petri::proxy::router(state);

    let protected = protected
        .merge(ws_router)
        .merge(webhook_router)
        .merge(auth_router)
        .merge(cloud_layer_router)
        .merge(petri_proxy);

    let swagger = SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", api_spec);

    // Public surface (unauthenticated): `/healthz` first, then everything
    // else gated by auth. SPA fallback comes last so handler routes win.
    let app = if let Some(dir) = frontend_dir {
        let path = PathBuf::from(dir);
        let index = path.join("index.html");
        let spa = ServeDir::new(&path).fallback(ServeFile::new(&index));
        public.merge(protected).merge(swagger).fallback_service(spa)
    } else {
        public.merge(protected).merge(swagger)
    };

    app.layer(build_cors_layer(&cors_config))
        .layer(TraceLayer::new_for_http())
}

/// CORS that permits the configured frontend origins to send the
/// Authorization header. When no origins are configured, falls back to
/// `Any` (dev-only — paired with `auth.mode = "dev_noop"`).
fn build_cors_layer(cfg: &AppConfig) -> CorsLayer {
    let methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
    ];
    let headers = [header::AUTHORIZATION, header::CONTENT_TYPE];

    if cfg.auth.cors_origins.is_empty() {
        return CorsLayer::new()
            .allow_origin(AllowOrigin::any())
            .allow_methods(methods)
            .allow_headers(headers);
    }

    let origins: Vec<HeaderValue> = cfg
        .auth
        .cors_origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_credentials(true)
        .allow_methods(methods)
        .allow_headers(headers)
}

/// Build the OpenAPI document without booting any state — used by the CLI's
/// `mekhan openapi` subcommand to dump the spec for codegen pipelines. Merges
/// the public (`/healthz`) and protected halves so the published spec is a
/// single document.
pub fn openapi_spec() -> utoipa::openapi::OpenApi {
    let ((_, mut api), (_, public)) = collect_openapi_parts();
    api.merge(public);
    api
}
