use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Mekhan API",
        version = env!("CARGO_PKG_VERSION"),
        description = "SOP workflow management — Petri-net backed, real-time collaborative editing.\n\n\
                       Routes are organized by domain: templates (the visual workflow editor's\
                       saved state), instances (running workflows), processes (HPI observability\
                       over running instances), catalogue (artifact registry), and provenance\
                       (token-level causality across nets).\n\n\
                       The JSON API lives under `/api/v1/*`; `/healthz` is the unauthenticated\
                       liveness probe. The Yjs WebSocket endpoint at `/api/yjs/{template_id}`\
                       carries the collaborative editor's binary CRDT protocol and is\
                       intentionally not modeled here."
    ),
    servers(
        (url = "/", description = "Same-origin BFF (production single-origin posture)."),
        (url = "http://localhost:3100", description = "Local mekhan-service direct."),
        (url = "http://localhost:15173", description = "SvelteKit dev server (proxies /api/* to mekhan).")
    ),
    components(
        // SSE event payload types — not referenced from any handler signature
        // (the responses are `text/event-stream`), so we register them
        // explicitly so frontend codegen picks them up.
        schemas(
            crate::causality::live::LiveMetricEvent,
            crate::causality::live::LiveLogEvent,
            crate::causality::live::LiveArtifactEvent,
            crate::triggers::TerminalOutcome,
            // Backend registry DTOs — referenced via Vec<_> in the
            // GET /api/v1/backends handler so utoipa's auto-discovery
            // misses the nested types. Frontend codegen needs both.
            crate::backends::BackendDescriptor,
            crate::backends::DispatchMode,
            crate::backends::ResourceChannel,
            crate::backends::OutputAuthoring,
            // Node-type registry DTO — referenced via Vec<_> in the
            // GET /api/v1/node-types handler.
            crate::nodes::NodeDescriptor,
            // Phase B.9 — Resource CRUD DTOs. The handler bodies refer to
            // these directly but utoipa's auto-discovery only walks the
            // handler signature; nested types (e.g. ResourceTypeInfo
            // appears only inside Vec<_>) need explicit registration so
            // frontend codegen emits matching TS types.
            crate::models::resource::ResourceSummary,
            crate::models::resource::ResourceDetail,
            crate::models::resource::ResourceTypeInfo,
            crate::models::resource::CreateResourceRequest,
            crate::models::resource::UpdateResourceRequest,
            crate::models::resource::RotateResourceRequest,
            crate::models::resource::ResourceAuditEntry,
            // S3 (unified capacity model) — the trait-space axis vocabulary +
            // the named presets surfaced on `ResourceTypeInfo.capacity_presets`
            // for the `capacity` type. Reached only through nested `Option<Vec<_>>`,
            // so register them explicitly for frontend codegen.
            crate::models::capacity::Liveness,
            crate::models::capacity::Dispatch,
            crate::models::capacity::Exclusivity,
            crate::models::capacity::CapacityAmount,
            crate::models::capacity::Eligibility,
            crate::models::capacity::CapacityAxes,
            crate::models::capacity::CapacityPreset,
            // docs/20 — Asset layer DTOs. Request/response shapes for asset
            // types + assets + records. Several appear only inside `Vec<_>` or
            // as request bodies, so register them explicitly for frontend
            // codegen. The schema field language reuses `PortField` wholesale.
            crate::models::asset::ScopeKind,
            crate::models::asset::Cardinality,
            crate::models::asset::AssetTypeSummary,
            crate::models::asset::AssetTypeDetail,
            crate::models::asset::CreateAssetTypeRequest,
            crate::models::asset::UpdateAssetTypeRequest,
            crate::models::asset::AssetSummary,
            crate::models::asset::AssetDetail,
            crate::models::asset::CreateAssetRequest,
            crate::models::asset::ReplaceRecordsRequest,
            crate::models::asset::AssetUsageItem,
            crate::handlers::assets::AssetFileUploadResponse,
            crate::handlers::assets::CsvImportBody,
            crate::handlers::assets::AssetFileUpload,
            // The shared field language assets reuse — registered here because
            // the asset DTOs are the first explicit referents (ports reach
            // these transitively via template handler signatures).
            crate::models::template::PortField,
            crate::models::template::FieldKind,
            crate::models::template::SelectOption,
            // Phase 3 (B-model) — Job-template CRUD DTOs. Nested types
            // (CommonSpec / EscapeHatch / TemplateParameter inside the version
            // shapes; TemplateStaging inside the detail + Vec<_> response) are
            // only reachable through handler bodies, so register them explicitly
            // for frontend codegen.
            crate::models::job_template::JobTemplateSummary,
            crate::models::job_template::JobTemplateDetail,
            crate::models::job_template::JobTemplateVersion,
            crate::models::job_template::CommonSpec,
            crate::models::job_template::EscapeHatch,
            crate::models::job_template::TemplateParameter,
            crate::models::job_template::TemplateStaging,
            crate::models::job_template::CreateJobTemplateRequest,
            crate::models::job_template::UpdateJobTemplateRequest,
            crate::models::job_template::StageJobTemplateRequest,
            // Container-image materialization — per-(version × datacenter) row +
            // the explicit-target request body.
            crate::models::image_materialization::ImageMaterialization,
            crate::handlers::container_images::MaterializeRequest,
            // Phase 1 (Lab Runner Fleet) — runner + registration-token DTOs.
            // The list/get bodies refer to the summaries via PaginatedResponse<_>
            // / Vec<_>, which utoipa's auto-discovery doesn't fully walk, so
            // register them explicitly for frontend codegen.
            crate::models::runner::RunnerSummary,
            crate::models::runner::RunnerDetail,
            crate::models::runner::EnrollRequest,
            crate::models::runner::EnrolledRunner,
            crate::models::runner::RunnerNatsCreds,
            crate::models::runner::CreateRegistrationTokenRequest,
            crate::models::runner::CreatedRegistrationToken,
            crate::models::runner::RegistrationTokenSummary,
            // Phase 5 — live presence snapshot row. The handler returns it via
            // `Vec<_>`, which utoipa's auto-discovery doesn't fully walk, so
            // register it explicitly for frontend codegen.
            crate::models::runner::RunnerPresenceSnapshot,
            // Phase A (Grouped + Enrolled Workers) — the worker identity plane:
            // enroll / registration-token / list-detail DTOs. The list/get bodies
            // refer to the summaries via PaginatedResponse<_>, which utoipa's
            // auto-discovery doesn't fully walk, so register them explicitly for
            // frontend codegen (same treatment as the runner DTOs above).
            crate::models::worker::WorkerSummary,
            crate::models::worker::WorkerDetail,
            crate::models::worker::EnrollWorkerRequest,
            crate::models::worker::EnrolledWorker,
            crate::models::worker::WorkerNatsCreds,
            crate::models::worker::CreateWorkerRegistrationTokenRequest,
            crate::models::worker::CreatedWorkerRegistrationToken,
            crate::models::worker::WorkerRegistrationTokenSummary,
            // Phase 4 (typed capability registry) — capability type DTOs +
            // the CapabilityField sub-shape. The list/detail bodies refer to
            // these via PaginatedResponse<_> / nested Vec<_>, which utoipa's
            // auto-discovery doesn't fully walk, so register them explicitly
            // for frontend codegen.
            crate::models::capability::CapabilityField,
            crate::models::capability::CapabilityTypeSummary,
            crate::models::capability::CapabilityTypeDetail,
            crate::models::capability::CreateCapabilityTypeRequest,
            // Phase 4 (placement requirements) — the AutomatedStep `requirements`
            // sub-shape. Nested inside WorkflowNodeData::AutomatedStep (carried
            // over Yjs, not a direct request body), so register explicitly so
            // frontend codegen emits the matching TS types for the editor's
            // requirements authoring panel + the typed claim payload.
            crate::models::template::Requirements,
            crate::models::template::Constraint,
            crate::models::template::ConstraintOp,
            // Executor backend config DTOs — the JSON shape each AutomatedStep
            // backend's `spec.config` carries. Registered so the SPA's generic
            // schema-driven config form can read them off the OpenAPI document
            // (also surfaced inline on BackendDescriptor.config_schema). Every
            // transitively-referenced sub-type must be ToSchema too.
            aithericon_executor_backend_configs::http::HttpConfig,
            aithericon_executor_backend_configs::http::HttpMethod,
            aithericon_executor_backend_configs::http::AuthConfig,
            aithericon_executor_backend_configs::http::ResponseMode,
            aithericon_executor_backend_configs::llm::LlmConfig,
            aithericon_executor_backend_configs::llm::Provider,
            aithericon_executor_backend_configs::llm::Role,
            aithericon_executor_backend_configs::llm::ChatMessage,
            aithericon_executor_backend_configs::llm::ImageInput,
            aithericon_executor_backend_configs::llm::ResponseFormat,
            aithericon_executor_backend_configs::docker::DockerConfig,
            aithericon_executor_backend_configs::docker::PullPolicy,
            aithericon_executor_backend_configs::docker::ResourceLimits,
            aithericon_executor_backend_configs::process::ProcessConfig,
            aithericon_executor_backend_configs::python::PythonConfig,
            aithericon_executor_backend_configs::postgres::PostgresConfig,
            aithericon_executor_backend_configs::postgres::PgOperation,
            aithericon_executor_backend_configs::postgres::RlsContext,
            aithericon_executor_backend_configs::loki::LokiConfig,
            aithericon_executor_backend_configs::loki::LokiOperation,
            aithericon_executor_backend_configs::loki::LokiDirection,
            aithericon_executor_backend_configs::prometheus::PrometheusConfig,
            aithericon_executor_backend_configs::prometheus::PrometheusOperation,
            aithericon_executor_backend_configs::ros::RosConfig,
            aithericon_executor_backend_configs::ros::RosOperation,
            aithericon_executor_backend_configs::kreuzberg::KreuzbergConfig,
            aithericon_executor_backend_configs::kreuzberg::ExtractionMode,
            aithericon_executor_backend_configs::kreuzberg::OcrSettings,
            aithericon_executor_backend_configs::kreuzberg::PdfSettings,
            aithericon_executor_backend_configs::smtp::SmtpConfig,
            aithericon_executor_backend_configs::smtp::TemplateSource,
            aithericon_executor_backend_configs::smtp::AttachmentSpec,
            aithericon_executor_backend_configs::file_ops::FileOpsConfig,
            aithericon_executor_backend_configs::file_ops::Compression,
            aithericon_executor_backend_configs::file_ops::ProbeConfig,
            aithericon_executor_backend_configs::file_ops::CopyConfig,
            aithericon_executor_backend_configs::file_ops::MoveConfig,
            aithericon_executor_backend_configs::file_ops::DeleteConfig,
            aithericon_executor_backend_configs::file_ops::AnnotateConfig,
            aithericon_executor_backend_configs::file_ops::ListConfig,
            aithericon_executor_backend_configs::file_ops::StatConfig,
            aithericon_executor_storage_types::StorageConfig,
            aithericon_executor_storage_types::StorageBackend,
            aithericon_executor_storage_types::StorageCredentials,
            aithericon_executor_storage_types::RetryConfig,
            aithericon_executor_domain::LlmToolCall,
            aithericon_executor_domain::ToolSchema,
            aithericon_executor_domain::PhaseStatus,
            aithericon_executor_domain::Phase,
            aithericon_executor_domain::Progress,
            // Cluster management DTOs (docs/16 §9). `ClusterSummary` appears
            // only inside `ClustersResponse.clusters: Vec<_>`, so register it +
            // the response/action shapes explicitly for frontend codegen.
            crate::handlers::clusters::ClusterSummary,
            crate::handlers::clusters::ClustersResponse,
            crate::handlers::clusters::ClusterActionResponse,
            // Live-aggregated cluster accounting over the allocations table.
            crate::handlers::clusters::ClusterMetrics,
            crate::handlers::clusters::FleetMetrics,
            // Resource-grant (allocations) read DTO, shared by the instance-
            // allocations and cluster-leases endpoints (`LeaseResponse` is an
            // alias for it, so only the one schema needs registering).
            crate::models::responses::AllocationResponse,
            // Capacity aggregator DTOs (docs/23 + docs/24). `CapacitySummary`
            // appears only inside the `Vec<_>` response, so register it + the
            // tagged-live union + holder shape + the backend authority enum for
            // frontend codegen.
            crate::handlers::capacities::CapacitySummary,
            crate::handlers::capacities::CapacityLive,
            crate::handlers::capacities::GrantHolder,
            crate::models::capacity::CapacityBackend,
            // Model-pool DTOs (docs/28 + docs/29 P1). `ModelSetView` appears only
            // inside the `Vec<_>` list response, so register it + the state enum +
            // transition request + the runner-advertised entry shapes (the live
            // half of the loaded-set AND-gate) for frontend codegen.
            // `ApprovedModelConfig` is NOT here: it carries only schemars (it lives
            // in `aithericon_resources` so the `model_registry` descriptor schema
            // picks it up) and surfaces via `GET /api/v1/resources/types`, not the
            // utoipa component block.
            crate::models::model_pool::ModelSetView,
            crate::models::model_pool::AutoscaleView,
            crate::models::model_pool::AutoscalePolicyInput,
            crate::models::model_pool::ModelState,
            crate::models::model_pool::TransitionRequest,
            crate::models::model_pool::CreateModelRequest,
            crate::models::model_pool::LoadModelRequest,
            crate::models::runner::ModelEntry,
            crate::models::runner::ModelInterfaceKind,
            // Model-pool reconciliation (docs/31 Phase 0) — per-node engine
            // inventory read. The nested per-node / per-engine / per-adapter
            // shapes are reached only through `Vec<_>` in the
            // `GET /api/v1/fleet/engines` response, so register them explicitly
            // for frontend codegen.
            crate::handlers::fleet_engines::FleetEnginesResponse,
            crate::handlers::fleet_engines::NodeInventory,
            crate::handlers::fleet_engines::NodeEngine,
            crate::handlers::fleet_engines::LoadedAdapter,
            // Model-pool P4 (docs/29 §6') — replica-autoscaler Control-Plane read +
            // manual scale DTOs.
            crate::models::model_replicas::ModelReplicaRow,
            crate::models::model_replicas::ModelReplicaScaleRequest,
            // docs/31 Loop 1 — node-pool replica reconciliation row (read-only).
            crate::models::node_replicas::NodeReplicaRow,
            // Operator load/unload action — the model-command wire envelope.
            crate::runner_commands::ModelCommand,
            crate::runner_commands::LoadTarget,
            // Official model-catalog browse (operator model browser).
            crate::handlers::model_catalog::CatalogModel,
            crate::handlers::model_catalog::ModelCatalogResponse,
            // Model-pool P5 (docs/29 §7') — inference metering audit ledger.
            crate::models::inference_metering::InferenceRequestLogRow,
            // Legacy file migration (docs/32) — file_inventory DTOs. The list
            // body wraps `InventoryEntry` in `Paginated<_>` and the register
            // request nests `InventoryRegisterItem` inside `Vec<_>`, neither of
            // which utoipa's auto-discovery fully walks; `InventoryCount`
            // appears only inside `InventoryStats`. Register them explicitly
            // for frontend codegen.
            crate::inventory::model::InventoryEntry,
            crate::inventory::model::InventoryRegisterItem,
            crate::inventory::model::InventoryRegisterRequest,
            crate::inventory::model::InventoryRegisterResponse,
            crate::inventory::model::InventoryStats,
            crate::inventory::model::InventoryCount,
            // Reconcile (docs/32 §4/§5) — crawl-vs-baseline classification DTOs.
            crate::inventory::reconcile::ObservedItem,
            crate::inventory::reconcile::ReconcileCounts,
            crate::inventory::reconcile::DuplicateGroup,
            crate::inventory::reconcile::StatusCount,
            crate::inventory::reconcile::ReconcileSummary,
            crate::inventory::reconcile::OrphanDbRow,
            crate::inventory::handlers::ReconcileBatchRequest,
            crate::inventory::handlers::MarkCanonicalResponse,
        ),
    ),
    tags(
        (name = "templates", description = "Workflow template CRUD, versioning, publish, compile-to-AIR."),
        (name = "instances", description = "Running workflow instances deployed to the petri-lab engine."),
        (name = "executions", description = "AutomatedStep execution introspection — data-plane channel byte taps (out-of-band streaming channel payloads over JetStream)."),
        (name = "processes", description = "HPI process inspection — metrics, logs, tasks, artifacts."),
        (name = "processes-live", description = "SSE backfill + live streams for process metrics, logs, and artifacts."),
        (name = "tasks", description = "Human task lifecycle — list, complete, cancel."),
        (name = "catalogue", description = "Artifact catalogue, lineage, distinct-value filters."),
        (name = "inventory", description = "Legacy file migration (docs/32) — by-reference physical-copy registry (`file_inventory`), content-addressed to the catalogue via `content_hash`. Batched register (no bytes) + list/stats."),
        (name = "provenance", description = "Token ancestry walks and cross-net signal links."),
        (name = "files", description = "Per-template file upload/download (50 MB limit, S3-backed)."),
        (name = "triggers", description = "Workflow triggers — cron/catalog/lifecycle/webhook/manual entry points."),
        (name = "auth-tokens", description = "Embedded per-user automation tokens (Zitadel-backed PATs)."),
        (name = "resources", description = "Typed credential CRUD (`postgres`, `openai`, `s3`, `slack`, `google_oauth`). Workflows bind aliases to resources at launch; secrets live in Vault."),
        (name = "assets", description = "User-typed, curated static content (docs/20). Asset types are user-defined `PortField` schemas; assets are version-pinned, scope-owned collections of schema-validated JSONB records (+ S3 for File fields), consumed by nodes as staged inputs."),
        (name = "job-templates", description = "Versioned cluster job-spec entity (flavor-tagged `slurm`/`nomad`) — typed common spec + flavor escape hatch + declared parameters, staged onto datacenter resources. No secret coupling."),
        (name = "container-images", description = "Container image materialization onto datacenter clusters."),
        (name = "backends", description = "AutomatedStep backend registry — display metadata, default config, default output port, dispatch mode."),
        (name = "node-types", description = "Workflow node-type registry — per-variant display metadata, runtime kind, and protocol flags."),
        (name = "health", description = "Liveness probe."),
        (name = "workspaces", description = "Tenant boundaries — membership + member admin (Phase A2)."),
        (name = "folders", description = "Workspace-scoped hierarchical template grouping (single-parent tree) + tag/visibility surface + per-folder OpenAPI bundle."),
        (name = "me", description = "Per-session preferences — active workspace switcher."),
        (name = "users", description = "Directory lookups — email → OIDC subject resolver for member admin."),
        (name = "admin", description = "Operator-only maintenance — remove / reseed the built-in demo workflows."),
        (name = "clusters", description = "Multi-cluster scheduling control plane — live datacenter cluster clients (connection health, watcher state, active leases) + force-reconnect / drain (read-through of the engine `ClusterRegistry`)."),
        (name = "runners", description = "Lab Runner Fleet — workspace-scoped runners + GitLab-style enrollment. Public `POST /enroll` is authed by a `rt_` registration token in the body; runners then authenticate with a mekhan-native `rnr_` bearer (SHA-256 hash stored, works offline)."),
        (name = "workers", description = "Grouped + Enrolled Workers — the identity plane on the executor worker pool: enrolled, group-scoped, revocable workers that still PULL. Public `POST /enroll` is authed by a `wt_` registration token in the body; workers then authenticate with a mekhan-native `wkr_` bearer. A group is backed by a `capacity` resource with the `worker` preset. Plus the anonymous worker-pool coverage read."),
        (name = "capability-types", description = "Phase 4 — admin-curated, workspace-scoped typed capability registry. Runner-advertised capabilities (enroll) + step Requirements (publish) are typed against these. Create/revoke are cookie-only (browser admin boundary)."),
        (name = "capacities", description = "Capacity aggregator (docs/23 + docs/24) — the unified Control-Plane read: every `capacity` + `datacenter` resource classified by the SINGLE dispatch authority (`CapacityAxes::backend`) with live utilization (token holders / presence online / worker coverage / scheduler lease state)."),
        (name = "models", description = "Model-pool control plane (docs/28 + docs/29) — the loaded-set projection (operator-curated `model_states` AND-gated against live runner interface catalogs) that sources the editor model picker, plus the operator state-machine transition (`approved → loading → loaded → draining → unloaded`). Projection/control seam only: inference bypasses the engine net + presence net; no NATS subjects."),
    ),
)]
pub struct ApiDoc;
