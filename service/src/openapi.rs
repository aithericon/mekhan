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
        (url = "http://localhost:5173", description = "SvelteKit dev server (proxies /api/* to mekhan).")
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
        (name = "health", description = "Liveness probe."),
    ),
)]
pub struct ApiDoc;
