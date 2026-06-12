//! Where/how a step's job runs: [`DeploymentModel`], capacity/lease
//! bindings, retry policy and placement [`Requirements`].

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Delay applied between automated-step retry attempts.
///
/// `Immediate` re-dispatches at once. `Fixed` waits `base_delay_ms` before
/// every attempt. `Exponential` waits `base_delay_ms * 2^attempt` (attempt is
/// the zero-based retry index), capped by the engine's timer service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackoffKind {
    #[default]
    Immediate,
    Fixed,
    Exponential,
}

/// Retry behaviour for an `AutomatedStep` whose execution fails or times out.
///
/// On failure the compiler re-dispatches the job (a fresh executor submit)
/// while `retries < max_retries`, optionally after a `backoff` delay, then
/// routes the exhausted token to the node's error output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts after the initial run. `0` disables
    /// retries (a single failure routes straight to the error output).
    #[serde(rename = "maxRetries", default = "default_max_retries")]
    pub max_retries: u32,
    /// Delay strategy between attempts.
    #[serde(default)]
    pub backoff: BackoffKind,
    /// Base delay in milliseconds for `Fixed`/`Exponential`. Ignored for
    /// `Immediate`.
    #[serde(rename = "baseDelayMs", default)]
    pub base_delay_ms: u64,
}

fn default_max_retries() -> u32 {
    3
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: default_max_retries(),
            backoff: BackoffKind::Immediate,
            base_delay_ms: 0,
        }
    }
}

/// Where an `AutomatedStep`'s job runs. Internally tagged on the wire by
/// `mode`: `{"mode":"executor", ...}` or `{"mode":"scheduled", ...}`. Keep the
/// `mode` strings in lockstep with the `snake_case` derive.
///
/// `executor` vs `scheduled` is the physically-honest split: our own executor
/// daemon pool (jobs dispatched over the NATS work queue and pulled by the
/// long-running executor workers) vs an external cluster. Resource admission
/// *is* scheduling, so:
/// - a seeded-token (`liveness=seeded`) capacity admission lives under
///   [`DeploymentModel::Executor`]'s `capacity` (the body runs on our executor
///   pool holding the typed lease — R1–R3 machinery), and
/// - an external cluster is a `datacenter` resource bound under
///   [`DeploymentModel::Scheduled`]'s `scheduler` (docs/13), with `operation`
///   selecting submit (today's sbatch/dispatch) vs lease (R4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DeploymentModel {
    /// Dispatch to our executor daemon pool over the NATS work queue (jobs are
    /// pulled by the long-running executor workers — NOT in-process). `capacity:
    /// None` is the plain path: our worker pool is currently unbounded (no
    /// control plane gating concurrency yet), so a job runs as soon as a worker
    /// is free. `capacity: Some` adds BOUNDED admission on top — a seeded-token
    /// (`liveness=seeded`) capacity claim/register/release handshake so contended
    /// infrastructure (GPUs, lab machines, LLM slots) is admission-controlled by
    /// the Petri firing rule (R3). The bound alias MUST be a Tokens or Presence
    /// `capacity` resource — a `datacenter`
    /// belongs under [`DeploymentModel::Scheduled`].
    ///
    /// `group` is the orthogonal IDENTITY-PLANE coordinate (docs/23/24): an
    /// optional `capacity`-resource alias (the `worker` preset:
    /// `competing_consumer · pull · hold · fixed · partition`) that narrows the
    /// pull routing from `executor-<wire>` to `executor-<wire>/<group>` so only
    /// enrolled workers of that group compete for the step's jobs. It stays a
    /// COMPETING pull pool — the group is a second coarse routing coordinate, NOT
    /// a per-worker push partition. `None` ⇒ the unchanged literal
    /// `executor-<wire>` (byte-stable AIR). `group` is mutually exclusive with
    /// `capacity`: `capacity` is the presence-PUSH admission handshake (R3),
    /// `group` is a plain pull coordinate — a step cannot be both (the compiler
    /// rejects `Some` + `Some`).
    Executor {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capacity: Option<CapacityBinding>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<String>,
    },
    /// Lease through an external cluster. `scheduler` names a `datacenter`
    /// resource (docs/13). `job_template` selects the scheduler's parameterized
    /// job (e.g. `petri-mumax3-worker`).
    Scheduled {
        /// `datacenter` resource alias.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheduler: Option<String>,
        /// Legacy/manual native job NAME registered on the scheduler. When
        /// `job_template_ref` is `Some`, publish OVERWRITES this string with the
        /// referenced template's slug (the name Phase-4 staging registers the
        /// native job under), so lowering/engine always read a concrete name
        /// here regardless of which authoring path produced it.
        #[serde(rename = "jobTemplate")]
        job_template: String,
        /// Optional control-plane job-template REFERENCE (Phase 3, B-model).
        /// When `Some`, publish resolves+validates it against the step's
        /// resolved cluster (`resolve_job_templates`) and stamps the template's
        /// slug into `job_template`. `None` ⇒ the bare `job_template` string is
        /// used verbatim (legacy/manual path). The actual staging mechanism is
        /// Phase 4 — this field only drives resolve+validate at publish.
        #[serde(
            rename = "jobTemplateRef",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        job_template_ref: Option<TemplateRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resources: Option<ResourceConfig>,
    },
}

/// A pinned reference to a control-plane job template (Phase 3, B-model).
///
/// Lives on [`DeploymentModel::Scheduled::job_template_ref`]. At publish,
/// `resolve_job_templates` loads the `(template_id, version)` row, validates the
/// template's flavor against the step's resolved cluster flavor, and stamps the
/// template's slug into the sibling `job_template` string. The actual staging of
/// the native job onto the cluster is Phase 4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TemplateRef {
    /// `job_templates.id` — the logical template (workspace-scoped).
    pub template_id: Uuid,
    /// `job_template_versions.version` — the immutable version to bind.
    pub version: i32,
}

impl Default for DeploymentModel {
    /// Plain executor dispatch (no pool) — byte-identical to pre-feature
    /// behaviour, and the shape every existing template round-trips to (a bare
    /// `{"mode":"executor"}`, or an absent `deploymentModel` via the field's
    /// `#[serde(default)]`).
    fn default() -> Self {
        DeploymentModel::Executor {
            capacity: None,
            group: None,
        }
    }
}

/// Optional resource hints forwarded to the scheduler for a `Scheduled` step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_mhz: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<u32>,
}

/// A binding to a Tokens or Presence `capacity` resource for executor-pool admission (`docs/14`).
/// Lives under [`DeploymentModel::Executor`]'s `capacity`; its presence makes the
/// compiler wrap the executor body with a claim/register/release handshake
/// against the pool resource's backing net so the engine's firing rule provides
/// admission control + mutual exclusion for free.
///
/// `alias` is REQUIRED (the `Option` lives on `Executor.capacity`, expressing "no
/// capacity binding"). It resolves at publish through the resource machinery to a backing
/// net id + kind + claim/lease schemas; `request` is validated against the
/// kind's `claim_schema`. The well-known-global fallback from the prototype is
/// gone — a pooled step must name a Tokens or Presence `capacity` resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CapacityBinding {
    /// Which Tokens/Presence `capacity` resource (by workspace alias) to claim against.
    /// Required. Resolved at publish to a backing net id (`pool-<resource_id>`),
    /// kind, and claim/lease schemas.
    pub alias: String,
    /// Claim-schema-shaped request params (the kind's `claim_schema` in
    /// `aithericon_resources::pool`). Carried verbatim into the `ClaimRequest`
    /// and validated against the kind's `claim_schema`. `None` ⇒ the kind's
    /// default placement (e.g. one token).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<serde_json::Value>,
}

/// A binding to a capacity provider held across a [`WorkflowNodeData::LeaseScope`]:
/// EITHER a `datacenter` resource (a leased cluster allocation) OR a `presence`
/// `capacity` resource (a single lab runner held exclusively). Its presence makes
/// `lower_lease_scope` hoist the claim/grant/register/release handshake to scope
/// scope — ONE unit held across the whole interior, released exactly once on exit.
///
/// The `pool` alias resolves through the single dispatch authority
/// (`resolve_binding(.., LeaseHolder, ..)` → `axes_for_resource().backend()`): a
/// `datacenter` → `Scheduler` backend (`Lease__scheduler`); a presence `capacity`
/// → `Presence` backend (`Lease__presence`). The same claim/register/release
/// machinery applies to both — only the lease schema + the presence-only
/// `requirements` (cap-matching) differ.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaseBinding {
    /// Capacity provider alias (workspace alias) the scope holds a unit against —
    /// a `datacenter` OR a `presence` `capacity`. Resolved at publish to
    /// `pool-<resource_id>` + the backend's `Lease__<backend>` schema via
    /// `resolve_binding(.., LeaseHolder, ..)`.
    pub pool: String,
    /// Claim-schema-shaped request params. For a `datacenter`:
    /// `gpu_count`/`gpu_type`/`max_duration_secs` (validated against the
    /// datacenter kind's `claim_schema`). `None` ⇒ the provider's default
    /// placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<serde_json::Value>,
}

/// Phase 4 — placement Requirements authored on a PRESENCE-pooled
/// `AutomatedStep`. A set of typed [`Constraint`]s over the runner-advertised
/// `caps`. Empty `constraints` (the default) matches any pool unit. The engine
/// matcher (`satisfies(requirements, caps)`) AND-s every constraint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Requirements {
    /// AND-ed constraints. Empty ⇒ matches anything.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<Constraint>,
}

/// One placement constraint over a `<capability>.<field>` of a runner's
/// advertised caps. `op == Exists` ignores `value`; every other op compares the
/// present `caps[capability][field]` against `value` per [`ConstraintOp`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Constraint {
    /// Capability name — must be a defined `capability_type` in the workspace.
    pub capability: String,
    /// Field within that capability's typed schema.
    pub field: String,
    pub op: ConstraintOp,
    /// Comparison operand. Ignored when `op == Exists`. Defaults to `null`.
    #[serde(default)]
    pub value: serde_json::Value,
}

/// Comparison operator for a [`Constraint`]. Wire values are lowercase so they
/// match the engine `satisfies` matcher's op strings exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConstraintOp {
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    In,
    Exists,
}
