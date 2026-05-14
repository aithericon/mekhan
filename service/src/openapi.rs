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
                       The Yjs WebSocket endpoint at `/api/yjs/{template_id}` carries the\
                       collaborative editor's binary CRDT protocol and is intentionally not\
                       modeled here."
    ),
    components(
        // SSE event payload types — not referenced from any handler signature
        // (the responses are `text/event-stream`), so we register them
        // explicitly so frontend codegen picks them up.
        schemas(
            crate::causality::live::LiveMetricEvent,
            crate::causality::live::LiveLogEvent,
            crate::causality::live::LiveArtifactEvent,
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
        (name = "health", description = "Liveness probe."),
    ),
)]
pub struct ApiDoc;
