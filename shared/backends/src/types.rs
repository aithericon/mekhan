//! Wire-facing enums that travel between mekhan-service, core-engine, and
//! aithericon-executor-service. None of them depend on service-internal
//! types — they're stand-alone wire tags.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Discriminator selecting which executor backend handles an automated step.
///
/// Snake-case wire values: `"python"`, `"process"`, `"docker"`, `"http"`,
/// `"llm"`, `"file_ops"`, `"kreuzberg"`, `"surya"`, `"smtp"`,
/// `"catalogue_query"`.
///
/// This is the canonical OpenAPI discriminator, the Y.Doc-stored string in
/// production templates, and the executor's `ExecutionSpec.backend` value.
/// Both the mekhan compiler and the executor registry key off it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackendType {
    Python,
    Process,
    Docker,
    Http,
    Llm,
    FileOps,
    Kreuzberg,
    /// Surya OCR. Sibling OCR backend to Kreuzberg, running Surya's
    /// detection + recognition + layout predictors in a managed Python
    /// subprocess (`aithericon-executor-surya`). Surfaces per-word bounding
    /// boxes (normalised `0..1`) + a flattened reading-order word list so a
    /// downstream step can map extracted fields back to source-document
    /// coordinates.
    Surya,
    /// SMTP mailer with Tera-templated subject/body/recipients. Consumes an
    /// `smtp` resource binding for host/port/auth and produces a structured
    /// `outcome` envelope describing success or the precise failure mode.
    Smtp,
    /// Point-in-time read of the data catalogue. Does NOT dispatch an executor
    /// job — the compiler lowers it to the engine's registered
    /// `catalogue_lookup` effect (input port `query`, output `results`).
    CatalogueQuery,
    /// Resource-bound SQL against a workspace `postgres` resource. The bound
    /// connection is overlaid into the resolved config (`ConfigOverlay`); the
    /// backend builds/caches a `PgPool` keyed by connection identity. Inline-only
    /// (not schedulable). Produces structured `rows` / `row_count` /
    /// `rows_affected` output.
    Postgres,
}

impl ExecutionBackendType {
    /// Canonical snake_case wire string. Keep in lockstep with the
    /// `#[serde(rename_all = "snake_case")]` derive — these strings are what
    /// the executor and editor pass around at runtime.
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Process => "process",
            Self::Docker => "docker",
            Self::Http => "http",
            Self::Llm => "llm",
            Self::FileOps => "file_ops",
            Self::Kreuzberg => "kreuzberg",
            Self::Surya => "surya",
            Self::Smtp => "smtp",
            Self::CatalogueQuery => "catalogue_query",
            Self::Postgres => "postgres",
        }
    }

    /// Inverse of [`Self::as_wire_str`]. Returns `None` for any unknown
    /// wire tag — callers should treat that as a 404 / validation error
    /// rather than a fallback to a default backend.
    pub fn from_wire_str(s: &str) -> Option<Self> {
        match s {
            "python" => Some(Self::Python),
            "process" => Some(Self::Process),
            "docker" => Some(Self::Docker),
            "http" => Some(Self::Http),
            "llm" => Some(Self::Llm),
            "file_ops" => Some(Self::FileOps),
            "kreuzberg" => Some(Self::Kreuzberg),
            "surya" => Some(Self::Surya),
            "smtp" => Some(Self::Smtp),
            "catalogue_query" => Some(Self::CatalogueQuery),
            "postgres" => Some(Self::Postgres),
            _ => None,
        }
    }
}

impl std::fmt::Display for ExecutionBackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

/// How a resolved resource envelope reaches the running backend.
///
/// The executor and mekhan compiler both branch on this — the compiler to
/// decide what kind of borrow to emit, the executor to know whether to
/// merge the envelope into the runtime config (`ConfigOverlay`) or stage it
/// as a sidecar file (`StagedFile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceChannel {
    /// SMTP-style. Compiler emits a `ResourceEnvelope` borrow; the publisher
    /// stages `<alias>.json` as an `InputDeclaration`; the executor reads
    /// the file at run time via `load_resource::<T>`.
    StagedFile,
    /// LLM-style. The backend's `prepare()` reads `<alias>.json` and merges
    /// fields into the resolved config (per-step values win). The runtime
    /// never sees a separate envelope file — everything is in `resolved_config`.
    ConfigOverlay,
    /// Backend doesn't bind a workspace resource (Process, Docker, …).
    None,
}

/// Lowering mode — intrinsic to the backend, decided at the decl, NOT the
/// step. Orthogonal to `DeploymentModel` (Inline / Scheduled) which is a
/// per-step author choice on any `ExecutorJob` backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DispatchMode {
    /// Standard executor dispatch. The compiler emits an executor job; the
    /// step's `DeploymentModel` decides whether it's inline via NATS or
    /// dispatched to an external cluster.
    ExecutorJob,
    /// Engine builtin effect (e.g. CatalogueQuery → `catalogue_lookup`). The
    /// compiler skips executor lowering entirely and emits an effect handler
    /// invocation directly into the Petri transition.
    EngineEffect {
        #[serde(rename = "handler")]
        handler: &'static str,
    },
}
