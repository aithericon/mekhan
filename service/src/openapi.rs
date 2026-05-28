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
        (url = "http://localhost:13100", description = "Local mekhan-service direct."),
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
            crate::models::template::ReplyMode,
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
        ),
    ),
    tags(
        (name = "templates", description = "Workflow template CRUD, versioning, publish, compile-to-AIR."),
        (name = "instances", description = "Running workflow instances deployed to the petri-lab engine."),
        (name = "processes", description = "HPI process inspection — metrics, logs, tasks, artifacts."),
        (name = "processes-live", description = "SSE backfill + live streams for process metrics, logs, and artifacts."),
        (name = "tasks", description = "Human task lifecycle — list, complete, cancel."),
        (name = "catalogue", description = "Artifact catalogue, lineage, distinct-value filters."),
        (name = "provenance", description = "Token ancestry walks and cross-net signal links."),
        (name = "files", description = "Per-template file upload/download (50 MB limit, S3-backed)."),
        (name = "triggers", description = "Workflow triggers — cron/catalog/lifecycle/webhook/manual entry points."),
        (name = "auth-tokens", description = "Embedded per-user automation tokens (Zitadel-backed PATs)."),
        (name = "resources", description = "Typed credential CRUD (`postgres`, `openai`, `s3`, `slack`, `google_oauth`). Workflows bind aliases to resources at launch; secrets live in Vault."),
        (name = "backends", description = "AutomatedStep backend registry — display metadata, default config, default output port, dispatch mode."),
        (name = "node-types", description = "Workflow node-type registry — per-variant display metadata, runtime kind, and protocol flags."),
        (name = "health", description = "Liveness probe."),
        (name = "workspaces", description = "Tenant boundaries — membership + member admin (Phase A2)."),
        (name = "projects", description = "Workspace-scoped template grouping + tag/visibility surface + per-project OpenAPI bundle."),
        (name = "me", description = "Per-session preferences — active workspace switcher."),
        (name = "users", description = "Directory lookups — email → OIDC subject resolver for member admin."),
        (name = "admin", description = "Operator-only maintenance — remove / reseed the built-in demo workflows."),
    ),
)]
pub struct ApiDoc;
