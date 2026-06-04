#![allow(dead_code)]

pub mod auth;
pub mod backends;
pub mod catalogue;
pub mod causality;
pub mod compiler;

pub mod config;
pub mod db;
pub mod demos;
pub mod handlers;
pub mod lifecycle;
pub mod models;
pub mod nats;
pub mod nodes;
pub mod observability;
pub mod openapi;
pub mod petri;
pub mod process;
pub mod projections;
pub mod query;
pub mod runners_nats;
pub mod runners_presence;
pub mod s3;
pub mod scope;
pub mod triggers;
pub mod worker_coverage;
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
    pub runner_presence: crate::runners_presence::RunnerPresence,
    /// Worker-pool feature — shared handle to the worker backend-coverage
    /// tracker's in-memory map. Workers heartbeat on `worker.*.presence`
    /// advertising which `ExecutorJob` backends they serve; the coverage tasks
    /// (`crate::worker_coverage`) keep this TTL-swept. Publish reads through it to
    /// WARN (never fail) on backends covered by zero live workers. No HTTP route
    /// in v1 — held here for a future fleet-coverage UI.
    pub worker_coverage: crate::worker_coverage::BackendCoverage,
    /// Publish-time asset resolver (docs/20 §5). Materializes the pinned
    /// records of every node-bound asset into the JSON envelope the publish
    /// handler splices into the AIR (`__assets`) before persistence. The
    /// launcher never touches this — instances run against already-spliced AIR,
    /// symmetric with `resource_resolver`.
    pub asset_resolver: Arc<crate::petri::asset_resolver::AssetResolver>,
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
        .routes(routes!(handlers::templates::apply_template))
        .routes(routes!(handlers::templates::list_versions))
        .routes(routes!(handlers::templates::get_latest))
        .routes(routes!(handlers::templates::get_air))
        .routes(routes!(handlers::templates::compile_preview))
        .routes(routes!(handlers::templates::io_stubs))
        .routes(routes!(handlers::templates::compile_graph))
        .routes(routes!(handlers::templates::analyze_graph))
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
        .routes(routes!(handlers::task_stream::task_stream))
        .routes(routes!(process::handlers::get_task))
        .routes(routes!(process::handlers::complete_task))
        .routes(routes!(process::handlers::cancel_task))
        // Catalogue
        .routes(routes!(catalogue::handlers::list_entries))
        .routes(routes!(catalogue::handlers::stats))
        .routes(routes!(catalogue::handlers::stats_by_net))
        .routes(routes!(catalogue::handlers::lineage))
        .routes(routes!(catalogue::handlers::distinct_values))
        .routes(routes!(catalogue::handlers::distinct_jsonb_values))
        .routes(routes!(catalogue::handlers::download_artifact))
        .routes(routes!(catalogue::handlers::get_entry))
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
        .routes(routes!(
            handlers::assets::list_assets,
            handlers::assets::create_asset
        ))
        .routes(routes!(
            handlers::assets::get_asset,
            handlers::assets::delete_asset
        ))
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
        // Creation is out-of-band (seed / Zitadel-auto-provision);
        // these endpoints manage *members* of existing workspaces.
        .routes(routes!(handlers::workspaces::list_workspaces))
        .routes(routes!(handlers::workspaces::get_workspace))
        .routes(routes!(
            handlers::workspaces::list_members,
            handlers::workspaces::add_member
        ))
        .routes(routes!(handlers::workspaces::remove_member))
        // Projects (Phase A2) — M:N grouping of templates within a
        // workspace. Not an ACL boundary.
        .routes(routes!(handlers::projects::list_workspace_tags))
        .routes(routes!(
            handlers::projects::list_projects,
            handlers::projects::create_project
        ))
        .routes(routes!(
            handlers::projects::delete_project,
            handlers::projects::update_project
        ))
        .routes(routes!(handlers::projects::attach_template))
        .routes(routes!(handlers::projects::detach_template))
        // Template tags + visibility (Phase A2; GET added Phase B).
        .routes(routes!(
            handlers::projects::get_template_tags,
            handlers::projects::set_template_tags
        ))
        .routes(routes!(handlers::projects::set_template_visibility))
        .routes(routes!(handlers::projects::list_template_projects))
        // Per-project OpenAPI bundle (Phase B) — synthesized webhook spec
        // for SDK generators + API doc viewers.
        .routes(routes!(handlers::openapi_bundle::project_openapi_bundle))
        // Active-workspace switcher (Phase B) — per-session override cookie.
        .routes(routes!(
            handlers::me::set_active_workspace,
            handlers::me::clear_active_workspace
        ))
        // Email → OIDC subject resolver (Phase B) — for the member-admin UI.
        .routes(routes!(handlers::users::resolve_user_by_email))
}

pub fn build_router(state: AppState) -> Router {
    let frontend_dir = state.config.frontend_dir.clone();
    let cors_config = state.config.clone();

    // Versioning policy:
    //   - JSON API surface lives under `/api/v1/*` (path attrs embed the
    //     version directly).
    //   - `/healthz` sits at the root, outside auth, k8s-conventional.
    //   - The unversioned siblings (`/api/auth/*` OAuth bootstrap,
    //     `/api/yjs/{template_id}` WS, `/api/triggers/webhook/{slug}`) are
    //     NOT OpenAPI-modeled and have external contracts mekhan does not
    //     control.
    //
    // The protected OpenApiRouter holds every authenticated handler; the
    // public one holds only `/healthz`. Both contribute to the same
    // `api_spec` so the published OpenAPI document stays a single document.
    let (protected_router, mut api_spec) = build_protected_openapi_router().split_for_parts();
    let (public_router, public_spec) = build_public_openapi_router().split_for_parts();
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
            "/api/triggers/webhook/{slug}",
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
    let (_, mut api) = build_protected_openapi_router().split_for_parts();
    let (_, public) = build_public_openapi_router().split_for_parts();
    api.merge(public);
    api
}
