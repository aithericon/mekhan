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
    /// Resource-bound LogQL query against a Grafana Loki HTTP API. The bound
    /// `loki` resource (`base_url` + optional bearer token + optional
    /// `X-Scope-OrgID` tenant header) is overlaid into the resolved config
    /// (`ConfigOverlay`); the backend issues an in-process HTTP request from
    /// the executor daemon. Inline-only (not schedulable). Produces structured
    /// `entries` / `series` / `result_type` / `stats` output.
    Loki,
    /// Resource-bound PromQL query against a Prometheus HTTP API. The bound
    /// `prometheus` resource (`base_url` + optional bearer token + optional
    /// `org_id` tenant header) is overlaid into the resolved config
    /// (`ConfigOverlay`); the backend issues an in-process HTTP request from
    /// the executor daemon. Inline-only (not schedulable). Produces structured
    /// `result_type` / `series` / `samples` / `sample_count` / `scalar` /
    /// `stats` output.
    Prometheus,
    /// ROS (Robot Operating System) interaction over a rosbridge WebSocket.
    /// The connection is runner-local (the runner advertises a reachable
    /// rosbridge endpoint) rather than a workspace resource, so
    /// `resource_channel = None`. Inline-only (not schedulable). Operations:
    /// publish a topic, call a service, await a topic message, send an action
    /// goal. (P1 stub — the rosbridge client + typedef mapper land in P2.)
    Ros,
}

/// The NATS namespace prefix for the worker executor-job stream FAMILY. Every
/// `AutomatedStep` whose backend dispatches an executor job routes through a
/// GROUP partition on the parallel `executor-<wire>-grp` stream (see
/// [`ExecutionBackendType::executor_namespace_for_group`]); `executor-<wire>`
/// is the stream-family PREFIX both sides build `<prefix>-grp` from. It is no
/// longer a dispatch target on its own — the anonymous bare-`executor-<wire>`
/// Pool consumer has been retired (the unified single-stream model: there is no
/// anonymous worker path, every job is grouped).
///
/// The separator is a HYPHEN, not a dot: apalis-nats derives the JetStream
/// stream name as `{namespace}_{priority}`, and JetStream stream names cannot
/// contain `.`. A dotted `executor.python` would yield the invalid stream
/// `executor.python_high`. The hyphen matches the proven datacenter-lease
/// convention (`lease-<grant>`, which also sanitizes to hyphens). Backend
/// `wire_name`s themselves are `[a-z_]` (`file_ops`) — the underscore is valid
/// in both stream names and NATS subject tokens, so it is left as-is.
pub const EXECUTOR_NS_PREFIX: &str = "executor-";

/// Build the worker stream-family prefix for a backend's snake-case `wire_name`
/// (`executor-python`). String-keyed companion to
/// [`ExecutionBackendType::executor_namespace`] for callers (the executor's
/// `BackendMeta::wire_name` registration loop) that hold the wire tag rather
/// than the enum. This is the prefix the `-grp` partition stream is built from
/// ([`executor_pool_namespace_for_group`]), NOT a standalone dispatch target.
pub fn executor_pool_namespace(wire_name: &str) -> String {
    format!("{EXECUTOR_NS_PREFIX}{wire_name}")
}

/// Build the grouped (partitioned) worker namespace for a backend's snake-case
/// `wire_name` and a `group` PARTITION token: `executor-<wire>-grp/<group>`.
/// String-keyed companion to
/// [`ExecutionBackendType::executor_namespace_for_group`] — the executor's
/// registration loop holds the wire tag, not the enum, when it builds the
/// consumer bind for its group. `group` is the partition token (a worker-group
/// capacity-resource UUID string).
pub fn executor_pool_namespace_for_group(wire_name: &str, group: &str) -> String {
    format!("{}-grp/{group}", executor_pool_namespace(wire_name))
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
            Self::Loki => "loki",
            Self::Prometheus => "prometheus",
            Self::Ros => "ros",
        }
    }

    /// The worker stream-family PREFIX this backend's executor jobs are routed
    /// through: `executor-<wire>` (e.g. `executor-python`, `executor-loki`).
    ///
    /// This is NOT a dispatch target on its own — in the unified single-stream
    /// model every executor job rides a GROUP partition on the parallel
    /// `executor-<wire>-grp` stream ([`Self::executor_namespace_for_group`]).
    /// `executor-<wire>` is the prefix both the compiler (producer) and the
    /// executor daemon (consumer, via [`executor_pool_namespace`]) build `-grp`
    /// from, so it remains the single source of truth for the stream family.
    pub fn executor_namespace(&self) -> String {
        executor_pool_namespace(self.as_wire_str())
    }

    /// The grouped worker namespace for this backend and a `group` PARTITION
    /// token: `executor-<wire>-grp/<group>`. This is THE dispatch target in the
    /// unified single-stream model — EVERY executor job routes through a group
    /// (a step naming no group is stamped with its workspace's always-seeded
    /// "default" worker group), so there is no longer a bare `executor-<wire>`
    /// dispatch path. The group here is the partition token = a worker-group
    /// capacity-resource UUID string (`[0-9a-f-]`), which is workspace-safe by
    /// construction (two workspaces can both have a "default" group without
    /// colliding on a queue) and a valid JetStream stream + NATS subject token.
    ///
    /// All grouped jobs land on the one parallel `executor-<wire>-grp` stream:
    /// apalis-nats `split_namespace` reads the segment after the slash as the
    /// subject-partition token, so the engine routes a grouped job to
    /// `executor-<wire>-grp.<prio>.<group>.<exec>` on stream
    /// `executor-<wire>-grp_<prio>`. This is the D1 isolation decision
    /// (docs/24): a SINGLE parallel grouped stream — NOT a stream-per-group —
    /// bounded by backend count, mirroring the way ALL runners share one
    /// `runner-jobs` stream partitioned by id. `<group>` is the subject
    /// partition; many workers in the group share one durable and COMPETE
    /// (a coarse routing coordinate, NOT a per-worker push partition).
    pub fn executor_namespace_for_group(&self, group: &str) -> String {
        executor_pool_namespace_for_group(self.as_wire_str(), group)
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
            "loki" => Some(Self::Loki),
            "prometheus" => Some(Self::Prometheus),
            "ros" => Some(Self::Ros),
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
