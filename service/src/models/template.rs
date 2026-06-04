use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

// --- Database row ---

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct WorkflowTemplate {
    pub id: Uuid,
    pub name: String,
    pub description: String,

    // Version chain
    pub base_template_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub version: i32,
    pub is_latest: bool,

    // Publishing
    pub published: bool,
    pub published_at: Option<DateTime<Utc>>,
    pub published_by: Option<Uuid>,

    // Graph data
    pub graph: serde_json::Value,

    // Compiled AIR (populated on publish)
    pub air_json: Option<serde_json::Value>,

    // Per-node compiler sub-graph interface registry (populated on publish,
    // alongside `air_json`). Sidecar — *not* embedded in AIR. Parent compiles
    // that embed this template via a `SubWorkflow` node read this directly
    // (no string-shape filtering on the child's AIR) to find the child's
    // entry place + workflow-exit terminals. NULL on pre-prototype rows;
    // `resolve_subworkflow_air` falls back to the old filter in that case.
    pub interface_json: Option<serde_json::Value>,

    // GitOps provenance — the git ref a `mekhan apply` published from
    // (shape: `SourceRef`). NULL for UI-published / new_version rows, so its
    // presence also marks a git-managed version. Stored raw to match the
    // `graph`/`air_json` `serde_json::Value` + `sqlx::FromRow` convention.
    pub source_ref: Option<serde_json::Value>,

    // Metadata
    pub author_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    // Workspace + visibility — added by migration 20240124. The handler-side
    // permission gate (`gate_template_read` / `gate_template_write`) reads
    // these directly off the row; the OpenAPI surface exposes them so the
    // frontend can render visibility badges and per-workspace filtering.
    pub workspace_id: Uuid,
    pub visibility: String,
    /// Owning parent family base id (`COALESCE(base_template_id, id)`), set
    /// only when `visibility == "private"`. A private sub-workflow may be
    /// embedded only by this family and never runs standalone.
    pub owner_template_id: Option<Uuid>,
}

impl WorkflowTemplate {
    /// Resolve this row's version-chain root: the family `base_template_id`
    /// when set, else `id` for a chain-root row (`COALESCE(base_template_id,
    /// id)`). The canonical way to derive a template's family id.
    pub fn chain_root_id(&self) -> Uuid {
        self.base_template_id.unwrap_or(self.id)
    }
}

// --- Visual editor data model (Section 2) ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowGraph {
    pub nodes: Vec<WorkflowNode>,
    pub edges: Vec<WorkflowEdge>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewport: Option<Viewport>,
    /// How concurrent fires (from triggers / manual / API) interact with
    /// already-running instances of this template. Defaults to `Unlimited`
    /// so existing templates load unchanged.
    ///
    /// Distinct from the per-`Trigger`-node `ConcurrencyPolicy` (which
    /// gates *fires* by Skip/Queue/DedupKey before they reach this
    /// template-level check). `InstanceConcurrencyPolicy` operates at the
    /// instance lifecycle layer — it sees a fire that already passed the
    /// per-trigger gate and decides whether to spawn now or coalesce.
    #[serde(default, skip_serializing_if = "is_default_instance_concurrency")]
    pub instance_concurrency: InstanceConcurrencyPolicy,
    /// Workflow-scoped reusable JSON-Schema fragments. Referenced from
    /// `executionSpec.config` (today: LLM `response_format.schema`) as
    /// `{"$ref": "#/definitions/<name>"}` and inlined at compile time by
    /// `compiler::schema_refs::inline_refs`. Local pointers only; external
    /// `$ref`s and JSON-Schema 2020-12 sibling-key merge semantics are
    /// rejected at validation. BTreeMap for byte-stable compile output.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub definitions: std::collections::BTreeMap<String, serde_json::Value>,
    /// Template-level default `datacenter` resource alias. A
    /// `Scheduled`/leased node whose own `scheduler` is absent inherits this
    /// (the second rung of the selection chain — node ?? template ??
    /// workspace ?? error; see `docs/16-multi-cluster-scheduling.md` §6). Lives
    /// on the graph JSON so it travels with the template + the Yjs doc.
    /// `None` = no template default (fall through to the workspace default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_scheduler: Option<String>,
}

fn is_default_instance_concurrency(c: &InstanceConcurrencyPolicy) -> bool {
    matches!(c, InstanceConcurrencyPolicy::Unlimited)
}

/// Template-level instance concurrency policy. Read by the trigger
/// dispatcher on fire and the lifecycle listener on instance terminal.
///
/// Tagged on the wire as `{"mode": "unlimited"}` / `{"mode":
/// "single_active_coalesce"}` so future variants (e.g. queue, locked)
/// can land without breaking the existing wire shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum InstanceConcurrencyPolicy {
    /// Every fire spawns a new instance. Default — matches legacy behaviour.
    #[default]
    Unlimited,
    /// At most one active instance at a time. While an instance is running,
    /// additional fires are *coalesced*: the dispatcher records that a fire
    /// was missed and dispatches exactly one follow-up after the active
    /// instance terminates. Right for state-mutating workflows whose body
    /// re-reads its inputs (e.g., BO retrain reading the catalogue) where
    /// a single follow-up absorbs N missed-while-busy fires.
    SingleActiveCoalesce,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Viewport {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WorkflowNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    /// Stable, author-facing namespace for guard references to this node's
    /// produced fields: a guard writes `<slug>.<field>` and the compiler
    /// rebinds it to this node's parked data place. Rhai-identifier-safe and
    /// unique within a graph. Optional on the wire — when absent the compiler
    /// derives a deterministic fallback from `id` (clean-cut: no stored
    /// templates to migrate). See [`WorkflowNode::slug`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    pub position: Position,
    pub data: WorkflowNodeData,
    /// Parent scope node id — child positions are relative to the parent.
    #[serde(rename = "parentId", skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Explicit width (used by scope nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    /// Explicit height (used by scope nodes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
}

// `ToolMeta` removed: agent tools are discovered structurally (target of
// an agent's `tools`-handle outgoing edge) and the LLM-facing
// `tool_name` / `tool_description` are derived from the node's own
// `data.label()` / `data.description()` rather than duplicated in a
// side-channel struct. The compiler slugifies the label via
// `sanitize_slug` to keep the name Rhai-identifier-safe.

impl WorkflowNode {
    /// Author-facing slug candidate: the explicit `slug` when set and
    /// non-blank, otherwise a deterministic Rhai-identifier-safe derivation
    /// from `id`. NOT guaranteed unique on its own — graph-wide uniqueness
    /// (and collision-suffixing of derived defaults) is enforced by the
    /// compiler's slug index, the single resolver.
    pub fn slug(&self) -> String {
        match self.slug.as_deref() {
            Some(s) if !s.trim().is_empty() => sanitize_slug(s),
            _ => sanitize_slug(&self.id),
        }
    }
}

/// Coerce an arbitrary string into a Rhai-identifier-safe slug matching
/// `^[a-z][a-z0-9_]*$`: lower-cased, every run of non-`[a-z0-9_]` collapsed to
/// a single `_`, leading/trailing `_` trimmed, a non-alphabetic head prefixed
/// with `n_`, and the empty result defaulted to `node`. Deterministic so a
/// missing slug derives stably from the node id.
pub fn sanitize_slug(raw: &str) -> String {
    let mut s = String::with_capacity(raw.len());
    let mut prev_us = false;
    for c in raw.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '_' {
            s.push(c);
            prev_us = c == '_';
        } else if !prev_us {
            s.push('_');
            prev_us = true;
        }
    }
    let s = s.trim_matches('_');
    if s.is_empty() {
        return "node".to_string();
    }
    match s.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => s.to_string(),
        _ => format!("n_{s}"),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

/// A node-level asset binding (docs/20 §5). Analogous to `resource_alias`:
/// the author picks an asset by its scope-resolved `ref_key`, and the compiler
/// stages the asset's whole record collection as an ordinary input the node
/// reads under `alias` (`<alias>.json`). Business data never enters the control
/// token — the records ride the same staging machinery as file inputs.
///
/// Whole-collection granularity only in v1 (the node does its own lookup in
/// code). Author-picked-row / runtime-filter are deferred (docs/20 §9).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AssetBinding {
    /// The staged-input name the node code reads (`<alias>.json`). Must be a
    /// flat identifier so it doesn't collide with a producer slug / resource
    /// name / control-token field. Defaults to `ref_key` when the author
    /// doesn't override it.
    pub alias: String,
    /// The asset's scope-resolved flat ref-key (`steel`, `materials_db`).
    /// Resolved at publish time through the scope resolver to a stable
    /// `(asset_id, version)` pin baked into the AIR.
    #[serde(rename = "refKey")]
    pub ref_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkflowNodeData {
    #[serde(rename = "start")]
    Start {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Declared input schema. The token this Start emits has this shape.
        /// Defaults to an empty-fields port so pre-typed-ports templates load
        /// unchanged. Replaces the previous opaque `initialData` blob; initial
        /// tokens are now seeded per-Start at instance creation time via the
        /// `start_tokens` field of `CreateInstanceRequest`.
        #[serde(default = "default_initial_port")]
        initial: Port,
        /// Optional process-name template. When set, the Start compiles an
        /// extra `process_start` effect so the instance registers a named
        /// HPI process. Supports `{{ field }}` placeholders resolved against
        /// the Start input token at run time, e.g. `"Invoice {{ invoice_id }}"`.
        /// Unset (the default) keeps the original single-place Start with no
        /// process registration.
        #[serde(
            rename = "processName",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        process_name: Option<String>,
    },
    #[serde(rename = "end")]
    End {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Declared terminal token shape. Defaults to an empty port (accepts
        /// any incoming token) so existing End nodes keep working. UI editor
        /// for `terminal` lands in Phase 4.
        #[serde(default = "default_terminal_port")]
        terminal: Port,
        /// Optional success-result binding. Each entry's `expression` is a
        /// Rhai expression over the inbound token; together they assemble the
        /// structured `value` of the success envelope (`{ ok: true, value }`)
        /// stamped onto the terminal token's `exit_code`. Empty (the default)
        /// inserts no transition — the terminal token is byte-identical to
        /// pre-feature behavior and the instance `result` stays NULL.
        #[serde(
            rename = "resultMapping",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        result_mapping: Vec<FieldMapping>,
    },
    #[serde(rename = "human_task")]
    HumanTask {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(rename = "taskTitle")]
        task_title: String,
        #[serde(rename = "instructionsMdsvex", skip_serializing_if = "Option::is_none")]
        instructions_mdsvex: Option<String>,
        steps: Vec<TaskStepConfig>,
        /// Opt-in: source the form block list at RUNTIME from a producer-namespaced
        /// `<slug>.<field>` reference instead of the static `steps` literal.
        #[serde(rename = "stepsRef", default, skip_serializing_if = "Option::is_none")]
        steps_ref: Option<String>,
    },
    #[serde(rename = "automated_step")]
    AutomatedStep {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(rename = "executionSpec")]
        execution_spec: ExecutionSpecConfig,
        /// Declared input shape. Empty by default — empty `fields` means
        /// "accepts any token" (Json pass-through), preserving back-compat
        /// for templates that wire arbitrary upstream output into an
        /// automated step. Phase 4 may derive richer defaults.
        #[serde(default = "default_automated_input_port")]
        input: Port,
        /// Declared output shape. Persisted as-is once authored; the bare
        /// default has no fields and should be re-derived from
        /// `execution_spec.backend_type` via `default_output_port` whenever a
        /// caller needs the canonical backend shape.
        #[serde(default = "default_automated_output_port")]
        output: Port,
        /// Retry behaviour on execution failure/timeout. Defaults to 3
        /// immediate retries (the historical hardcoded value), so existing
        /// templates keep their prior semantics without re-authoring.
        #[serde(rename = "retryPolicy", default)]
        retry_policy: RetryPolicy,
        /// Where/how the job is dispatched. `Executor` (default) = our executor
        /// daemon pool over the NATS work queue, optionally under a Tokens or
        /// Presence `capacity` admission (`Executor.capacity`). `Scheduled` =
        /// lease through an external cluster (a `datacenter` resource, docs/13).
        /// `#[serde(default)]` + the `Executor` default ⇒ every existing
        /// template round-trips unchanged (same precedent as `retry_policy`).
        ///
        /// Resource admission *is* scheduling, so the former standalone
        /// `resourcePool` field folded into `Executor.capacity` here (post-R3
        /// consolidation pivot).
        #[serde(rename = "deploymentModel", default)]
        deployment_model: DeploymentModel,
        /// PROTOTYPE — opt-in streaming side-channel. When `true`, the node
        /// exposes a second output port "stream" and the compiler synthesizes a
        /// Signal place `p_{id}_stream` that receives ONE token per executor
        /// `EventCategory::Log` event (Python `log_info()/log_debug()/…`). An
        /// edge from the "stream" handle fires the downstream node once per log
        /// token; the normal "out" control token still governs termination.
        /// Plain `bool` + `#[serde(default)]` ⇒ existing templates (field
        /// absent → `false`) round-trip unchanged (same precedent as
        /// `retry_policy`/`deployment_model`).
        #[serde(rename = "streamOutput", default)]
        stream_output: bool,
        /// Opt-in streaming CONSUMER. When `true`, the node exposes a second
        /// INPUT port "stream" and becomes a long-lived stateful reducer: it is
        /// seeded at net entry, receives the upstream producer's chunks over IPC
        /// (`aithericon.chunks()`), and folds them in-process. Wire the
        /// producer's `stream` handle to this node's `stream` input and its
        /// control `out` to this node's `in` (the control token's arrival is the
        /// end-of-stream / EOF trigger, carrying `stream_count`). The compiler
        /// derives the executor `feed_chunks` flag from this. Plain `bool` +
        /// `#[serde(default)]` ⇒ existing templates round-trip unchanged.
        #[serde(rename = "streamInput", default)]
        stream_input: bool,
        /// Phase 4 — placement Requirements on a PRESENCE-pooled step. A set of
        /// typed [`Constraint`]s over the pool unit's advertised `caps` (the
        /// `runners.capabilities` blob keyed by capability name). At claim time
        /// the compiler injects these into the claim payload as a Rhai literal
        /// and the presence pool's `t_grant` guard
        /// (`satisfies(claim.requirements, unit.caps)`) admits ONLY a runner
        /// whose caps satisfy every constraint. `None` (the default) ⇒ no
        /// placement constraint (matches any unit) and the claim carries an
        /// empty `#{ constraints: [] }`. Ignored on seeded / Scheduled /
        /// inline deployments (the claim there is unchanged; publish-time
        /// validation still checks the constraint shapes). Plain
        /// `#[serde(default, skip_serializing_if)]` ⇒ existing templates
        /// round-trip unchanged (same precedent as `retry_policy` /
        /// `deployment_model`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requirements: Option<Requirements>,
        /// Node-level asset bindings (docs/20 §5). Each entry stages an asset's
        /// whole record collection as an ordinary input (`<alias>.json`) the
        /// node code reads. `#[serde(default)]` ⇒ existing templates (field
        /// absent → empty) round-trip unchanged (same precedent as
        /// `deployment_model`/`stream_output`).
        #[serde(rename = "assetBindings", default, skip_serializing_if = "Vec::is_empty")]
        asset_bindings: Vec<AssetBinding>,
    },
    #[serde(rename = "decision")]
    Decision {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        conditions: Vec<BranchCondition>,
        /// Otherwise/else branch handle id. The wire shape is `Option<String>`
        /// for forward-compat with future multi-default-branch decisions, but
        /// today the only accepted value is `DEFAULT_BRANCH_HANDLE_ID`
        /// (`"default"`) — both the editor's xyflow Handle id and the
        /// compiler's default output place use that literal, so any other
        /// value would render as a floating edge in the editor and is
        /// rejected at compile time (see `compiler::validate`).
        #[serde(rename = "defaultBranch", skip_serializing_if = "Option::is_none")]
        default_branch: Option<String>,
    },
    #[serde(rename = "parallel_split")]
    ParallelSplit {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Unified converge primitive. `mode == All` is the AND-join: waits for
    /// every incoming branch and merges payloads per `merge_strategy`.
    /// `mode == Any` is the canonical petri-net XOR-join (dual of `Decision`'s
    /// XOR-split) — fires per arriving token. Both modes park each branch's
    /// inbound token in `p_<id>_data` so downstream `<slug>.<field>` borrows
    /// resolve through the standard read-arc pipeline (the `output` Port
    /// declares the addressable shape).
    #[serde(rename = "join")]
    Join {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// `All` (AND-join) waits for every incoming branch. `Any` (XOR-join)
        /// fires per arriving token.
        #[serde(default)]
        mode: JoinMode,
        /// Honoured only when `mode == All`. For `Any` only one payload ever
        /// arrives per firing, so there is nothing to merge.
        #[serde(
            rename = "mergeStrategy",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        merge_strategy: Option<MergeStrategy>,
        /// Declared output shape. Each branch's inbound payload is parked at
        /// `p_<id>_data`; the declared fields here describe what downstream
        /// `<slug>.<field>` borrows can read.
        #[serde(default = "default_join_output_port")]
        output: Port,
    },
    #[serde(rename = "loop")]
    Loop {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(rename = "maxIterations")]
        max_iterations: i32,
        #[serde(rename = "loopCondition")]
        loop_condition: String,
        /// Optional fold/scan state carried across iterations. Each is an
        /// additional field in the loop's parked `p_<id>_data` envelope
        /// (alongside `iteration`): initialized in the enter transition,
        /// re-folded write-once-per-iteration in the continue transition.
        /// Downstream blocks read them via `<loop_slug>.<var>` borrows.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        accumulators: Vec<LoopAccumulator>,
    },
    #[serde(rename = "scope")]
    Scope {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Container that holds ONE datacenter allocation for the duration of its
    /// body. Decouples "hold an allocation" from "loop": any AutomatedStep
    /// (deployment `Scheduled { Submit }`) nested inside a LeaseScope — directly
    /// or through intervening containers like a plain Loop — runs ON the held
    /// allocation by containment (no per-step `run_on_lease` flag). The lease is
    /// acquired once on enter and released once on exit; the held lease (incl.
    /// `executor_namespace` / `alloc_id`) is parked into the scope's
    /// `p_<id>_data` envelope under a `lease` key, so body steps and downstream
    /// blocks borrow `<scope_slug>.lease.<field>` through the standard read-arc
    /// pipeline. Children attach via the same `body_in`/`body_out` interior
    /// handles as Loop (`parent_id == lease_scope.id`); the perimeter `in`/`out`
    /// handles connect to the outer flow.
    ///
    /// To hold ONE cluster allocation across loop iterations, compose
    /// `LeaseScope { Loop { … } }` — the scope acquires before the loop starts
    /// and releases after it exits.
    #[serde(rename = "lease_scope")]
    LeaseScope {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// REQUIRED datacenter lease binding (a LeaseScope with no lease is a
        /// pointless empty container). Reuses [`LeaseBinding`] verbatim — the
        /// `scheduler` alias resolves via `resolve_binding(..., "datacenter",
        /// ...)` exactly as the Loop-lease path does — and is NOT `Option`;
        /// `validate_lease_scope` rejects an empty `scheduler` alias.
        lease: LeaseBinding,
    },
    /// Dynamic data-parallel map-reduce. Scatters the collection at `itemsRef`
    /// into N item tokens (one per element), runs a BODY sub-graph of child
    /// nodes (`parent_id == map.id`, attached via the same `body_in`/`body_out`
    /// handle mechanism as Loop) once per element, gathers the N results, and
    /// reduces them to one collection token parked at `p_<id>_data`. Downstream
    /// blocks borrow the gathered collection as `<map_slug>[*].<field>` (the
    /// Repeater `[*]` machinery generalized to any Array-typed parked producer;
    /// `<map_slug>.<field>` without `[*]` is a hard `MapRefMissingStar` error).
    /// The scatter writes the item namespace ONTO each body token (namespace-
    /// on-token, v1) so body guards / Python read `item.<field>` directly.
    #[serde(rename = "map")]
    Map {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Producer-namespaced reference to the array to scatter, carrying
        /// exactly one `[*]` boundary at iteration time (resolved through the
        /// Repeater items-ref machinery), e.g. `extract.tasks`. IGNORED when
        /// `stream_source` is set (a streaming Map sources elements from its
        /// `stream`/`control` edges, not a static array).
        #[serde(rename = "itemsRef", default)]
        items_ref: String,
        /// When `true`, this Map is a STREAMING map: instead of scattering the
        /// static `items_ref` array, it ingests a streaming producer's chunks
        /// (one element per chunk over the `stream` handle) and sizes its gather
        /// barrier on the runtime `stream_count` (from the `control` handle).
        /// Parallel-only — bodies fan out concurrently exactly like the array
        /// path; `__map_idx` (the producer sequence) restores order at the
        /// gather. Plain `bool` + `#[serde(default)]` ⇒ array-source Maps
        /// round-trip unchanged.
        #[serde(rename = "streamSource", default)]
        stream_source: bool,
        /// Identifier the per-item element is bound to on each body token.
        /// Body guards / Python read `<item_var>.<field>`. Defaults to `item`.
        #[serde(rename = "itemVar", default = "default_item_var")]
        item_var: String,
        /// Field on each body output token whose value becomes the gathered
        /// element (the per-iteration result the reduce collects).
        #[serde(rename = "resultVar")]
        result_var: String,
        /// Declared shape of one gathered element. Empty fields ⇒ `Any`
        /// element. Drives the `<map_slug>[*].<field>` borrow surface.
        #[serde(skip_serializing_if = "Option::is_none")]
        output: Option<Port>,
    },
    /// Stream fold — drains a streaming producer's per-call `set_output` chunks
    /// and folds them into ONE output token via a declarative `reduce` strategy,
    /// gating completion behind an end-of-stream counted barrier sized by the
    /// producer's `stream_count`. No body, no executor: the fold is pure Rhai in
    /// the net. This is the clean extraction of the old `StreamConsumer`'s
    /// default `Rhai` dispatch; the body-dispatch streaming patterns live
    /// elsewhere now — per-chunk parallel map → a streaming-source `Map`;
    /// stateful in-process reduce → an `AutomatedStep` with `streamInput`. Parks
    /// the reduced output write-once at `p_<id>_data` like Map.
    #[serde(rename = "stream_fold")]
    StreamFold {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Name of the output field that holds the reduced result; borrowable
        /// downstream as `<slug>.<resultVar>`.
        #[serde(rename = "resultVar", default = "default_stream_result_var")]
        result_var: String,
        /// How the drained chunks are folded into the single output token.
        /// Defaults to an ordered `Array` (sort by stream sequence, project
        /// `.value`).
        #[serde(default)]
        reduce: StreamReduce,
    },
    /// Pass-through control node that marks a named phase on the owning HPI
    /// process. Compiles to a shape transition (forwards the workflow token
    /// unchanged + emits an `executor-phase`-shaped breadcrumb) followed by a
    /// `process_log_message` effect. The causality consumer projects it into
    /// `hpi_processes.config.progress.phases`. Effective only when an upstream
    /// Start registered a process (`processName`); otherwise a silent no-op.
    #[serde(rename = "phase_update")]
    PhaseUpdate {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Phase name. Supports `{{ field }}` placeholders resolved against the
        /// inbound token at run time.
        #[serde(rename = "phaseName")]
        phase_name: String,
        /// Status to set on the phase. Defaults to `running`.
        #[serde(default)]
        status: PhaseUpdateStatus,
        /// Optional phase message. Supports `{{ field }}` placeholders.
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// Pass-through control node that sets the owning HPI process's progress
    /// fraction (and optional message / step counts). Compiles to a shape
    /// transition + a `process_log_metric` effect keyed `progress_fraction`,
    /// projected into `hpi_processes.config.progress`. Effective only within a
    /// named process; otherwise a silent no-op.
    #[serde(rename = "progress_update")]
    ProgressUpdate {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Overall completion fraction, 0.0–1.0.
        fraction: f64,
        /// Optional progress message. Supports `{{ field }}` placeholders.
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        #[serde(
            rename = "currentStep",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        current_step: Option<i64>,
        #[serde(
            rename = "totalSteps",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        total_steps: Option<i64>,
    },
    /// Pass-through control node that marks the owning HPI process `failed`
    /// with a templated message. Compiles to a shape transition (forwards the
    /// workflow token unchanged + emits a `#{ reason }` breadcrumb) followed
    /// by the `process_fail` builtin effect. The net keeps running to its
    /// normal End — this is a process-level marker, not a net kill-switch.
    /// Effective only within a named process (`processName` on an upstream
    /// Start); otherwise a silent no-op.
    #[serde(rename = "failure")]
    Failure {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Failure message. Supports `{{ field }}` placeholders resolved
        /// against the inbound token at run time.
        #[serde(rename = "failureMessage", skip_serializing_if = "Option::is_none")]
        failure_message: Option<String>,
        /// Optional error-result binding. Each entry's `expression` is a Rhai
        /// expression over the inbound token; together they assemble the
        /// structured `error.value` of the error envelope
        /// (`{ ok: false, error: { reason, value } }`) stamped onto the
        /// token's `exit_code` and carried through to the terminal End.
        #[serde(
            rename = "errorResultMapping",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        error_result_mapping: Vec<FieldMapping>,
    },
    /// Fire-and-forget delay. Waits `durationMsExpr` milliseconds then
    /// forwards the input token on its single output. Compiles to the engine's
    /// `timer_schedule` effect (see `ctx.delay()` in
    /// `engine/sdk/src/context.rs`). `durationMsExpr` is a Rhai expression so
    /// the delay can be data-driven from upstream refs (`<slug>.<field>`).
    #[serde(rename = "delay")]
    Delay {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Rhai expression evaluated against the inbound token at runtime.
        /// Must return an integer number of milliseconds. Examples:
        /// `"5000"`, `"order.sla_ms"`, `"input.timeout * 1000"`.
        #[serde(rename = "durationMsExpr")]
        duration_ms_expr: String,
    },
    /// Body-container that races a wrapped subgraph against a deadline.
    /// Body work flows out the `body_in` source handle; the body's terminal
    /// edge targets `body_out`. Two outputs: `default` (the "done" path —
    /// body completed in time, timer cancelled) and `timeout` (timer fired
    /// first; in-flight body work in cancellable children is also drained
    /// via per-kind cancel effects).
    #[serde(rename = "timeout")]
    Timeout {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Rhai expression evaluated against the inbound token at runtime.
        /// Must return an integer number of milliseconds. Same shape as
        /// `Delay.duration_ms_expr`.
        #[serde(rename = "durationMsExpr")]
        duration_ms_expr: String,
    },
    /// Trigger node (Phase 5). Lives at the template level and connects to a
    /// target input port via a single outgoing edge. Triggers are never edge
    /// targets; they are *inputs to the workflow*, not part of it. AIR
    /// compilation skips trigger nodes — they are a pre-compile concern owned
    /// by the dispatcher (`service::triggers`).
    #[serde(rename = "trigger")]
    Trigger {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Tagged source describing what event fires this trigger.
        source: TriggerSource,
        /// Concurrency / dedup policy applied by the dispatcher.
        #[serde(default)]
        concurrency: ConcurrencyPolicy,
        /// Per-target-field mapping. Each entry's `expression` is a Rhai
        /// expression evaluated against the trigger source's event payload.
        #[serde(rename = "payloadMapping", default)]
        payload_mapping: Vec<FieldMapping>,
        /// Disabled triggers are stored but the dispatcher ignores them.
        #[serde(default)]
        enabled: bool,
        /// Pre-AIR direct target. When set, the trigger fires by seeding the
        /// named AIR place with the supplied payload, bypassing graph-edge
        /// resolution. Mutually exclusive with an outgoing edge in the graph:
        /// pre-AIR templates carry a Trigger-only stub graph (no Start, no
        /// edges). Used by clinic-style headless templates pushed through
        /// `POST /api/templates/apply-air`.
        #[serde(
            rename = "airTargetPlaceId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        air_target_place_id: Option<String>,
    },
    /// Agent block — one LLM call, optionally extended with tool children
    /// and a multi-turn loop. PR 1 only models the type; the degenerate
    /// single-turn path lowers byte-identically to `AutomatedStep(Llm)`. The
    /// full agent-loop lowering (parked state place + dispatch/collect per
    /// tool + turn counter) lands in a follow-up PR (see
    /// `docs/12-agent-node-design.md` § 3).
    ///
    /// Tools are child nodes of this container in a future PR (tagged via a
    /// `tool_meta` field on `WorkflowNodeData`); PR 1 ignores children
    /// structurally and rejects non-degenerate shapes with
    /// `CompileError::Compilation`.
    #[serde(rename = "agent")]
    Agent {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// LLM model + provider selection. Same shape the existing
        /// `LlmConfig` carries in `executionSpec.config`; the degenerate
        /// path uses these fields verbatim when constructing the equivalent
        /// `LlmConfig` payload.
        model: ModelRef,
        /// Optional system prompt template (supports `{{<slug>.<field>}}`
        /// placeholders, same as the LLM `system_prompt` config field).
        #[serde(
            rename = "systemPrompt",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        system_prompt: Option<String>,
        /// Initial user prompt template (supports `{{<slug>.<field>}}`
        /// placeholders, corresponds to `LlmConfig::prompt`).
        #[serde(rename = "userPrompt")]
        user_prompt: String,
        /// Optional response-format constraint (`{"type": "text"}` or
        /// `{"type": "json_schema", "schema": {...}}`). Opaque JSON in the
        /// model layer — the executor LLM backend validates it.
        #[serde(
            rename = "responseFormat",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        response_format: Option<serde_json::Value>,
        /// Vision inputs attached to the user message — each `{"path":
        /// "{{<slug>.<field>}}", "media_type"?: "..."}`. Opaque JSON in the
        /// model layer (same as `response_format`); the executor LLM backend
        /// validates it and the compiler's LLM `ref_scanner` walks
        /// `images[i].path` for `{{<slug>.<field>}}` borrows exactly as it
        /// does for a single-shot LLM step. Empty by default. Carries the
        /// vision capability that lets the Agent fully subsume the retired
        /// LLM step.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<serde_json::Value>,
        /// Hard cap on agent turns. `1` (default) is the single-shot LLM
        /// call indistinguishable from `AutomatedStep(Llm)` — the degenerate
        /// path the equivalence test pins down.
        #[serde(rename = "maxTurns", default = "default_max_turns")]
        max_turns: u32,
        /// Optional terminal Rhai guard. When `Some`, the agent loop exits
        /// once this expression evaluates true on the parked agent state.
        /// Inert in the degenerate (single-turn) path.
        #[serde(rename = "stopWhen", default, skip_serializing_if = "Option::is_none")]
        stop_when: Option<String>,
        /// Context-window management strategy. Defaults to `None` (no
        /// compaction). Inert in the degenerate path.
        #[serde(rename = "contextStrategy", default)]
        context_strategy: ContextStrategy,
        /// What happens when a tool call fails. Defaults to `Feedback`.
        /// Inert in PR 1 (no tools).
        #[serde(rename = "onToolError", default)]
        on_tool_error: ToolErrorPolicy,
        /// Retry behaviour on a per-turn inference failure/timeout. Same shape
        /// and defaults as `AutomatedStep::retry_policy`. On the degenerate
        /// (single-shot) path this threads straight through to the synthesized
        /// `AutomatedStep(Llm)`. On the multi-turn loop path it caps the
        /// executor's per-turn `max_retries`.
        #[serde(rename = "retryPolicy", default)]
        retry_policy: RetryPolicy,
        /// Where/how each inference turn is dispatched — same field, defaults
        /// and semantics as `AutomatedStep::deployment_model`. On the
        /// degenerate single-shot path it reaches the full
        /// `Executor{capacity}` / `Scheduled{lease}` dispatch in
        /// `lower_automated_step`. The multi-turn loop path supports
        /// `Executor { capacity: None }` only in v1 and compile-rejects the rest
        /// (mirrors the `context_strategy` gate); per-turn pooled/scheduled
        /// admission is a follow-up (docs/12).
        #[serde(rename = "deploymentModel", default)]
        deployment_model: DeploymentModel,
        /// Node-level asset bindings (docs/20 §5) — same field, defaults and
        /// semantics as `AutomatedStep::asset_bindings`. The agent's inference
        /// turns read the staged asset(s) as ordinary inputs.
        #[serde(rename = "assetBindings", default, skip_serializing_if = "Vec::is_empty")]
        asset_bindings: Vec<AssetBinding>,
    },
    /// Calls another published template as a child net and returns its
    /// terminal result, correlated per invocation. Compiles (via
    /// `Context::spawn`) to: a parent-side request/spawn effect, a
    /// `bridge_out` carrying the mapped initial token to a freshly spawned
    /// child instance, and `bridge_in` reply/failure places joined back on a
    /// synthesized correlation key. The child template is resolved and its
    /// compiled AIR is embedded at the *parent's* publish time (see
    /// `version_pin`) so a later change to the child does not alter
    /// already-published parents.
    #[serde(rename = "sub_workflow")]
    SubWorkflow {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Stable identity of the child template family (any version row's
        /// `base_template_id`/`id` — resolution picks the concrete row per
        /// `version_pin` at publish time).
        #[serde(rename = "templateId")]
        template_id: Uuid,
        /// Which concrete version of the child to embed. `Latest` resolves the
        /// family's `is_latest` row *at the parent's publish time* and freezes
        /// that concrete version into the embedded AIR; `Pinned` freezes an
        /// explicit version. Either way the published parent is deterministic.
        #[serde(rename = "versionPin", default)]
        version_pin: VersionPin,
        /// Parent upstream token → child Start `initial` port fields. Each
        /// entry's `expression` is a Rhai expression over the inbound token;
        /// together they assemble the token bridged into the child. Empty
        /// (the default) passes the inbound token through unchanged.
        #[serde(
            rename = "inputMapping",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        input_mapping: Vec<FieldMapping>,
        /// Declared shape of the child's terminal result, mapped back onto the
        /// workflow token at the join. Empty fields ⇒ pass the child result
        /// through unchanged (Json). Authoring can prefill this from the
        /// child's End `terminal` port.
        #[serde(default = "default_subworkflow_output_port")]
        output: Port,
        /// Display-only snapshot of the child's **input** contract — its
        /// `Start { initial }` port. Reconciled at publish from the resolved
        /// child and refreshed by the editor's `/io-contract` fetch, exactly
        /// like `output`. The compiler re-derives the real child input from the
        /// frozen child, so this field never feeds compilation: it exists so the
        /// canvas can show "what this sub-workflow consumes" (the way a Start
        /// node shows its declared fields) without opening the property panel.
        /// Empty `in` port ⇒ not yet resolved / child declares no Start fields.
        #[serde(
            rename = "inputContract",
            default = "default_subworkflow_input_contract"
        )]
        input_contract: Port,
    },
}

impl WorkflowNodeData {
    pub fn label(&self) -> &str {
        match self {
            Self::Start { label, .. }
            | Self::End { label, .. }
            | Self::HumanTask { label, .. }
            | Self::AutomatedStep { label, .. }
            | Self::Agent { label, .. }
            | Self::Decision { label, .. }
            | Self::ParallelSplit { label, .. }
            | Self::Join { label, .. }
            | Self::Loop { label, .. }
            | Self::Scope { label, .. }
            | Self::LeaseScope { label, .. }
            | Self::Map { label, .. }
            | Self::StreamFold { label, .. }
            | Self::PhaseUpdate { label, .. }
            | Self::ProgressUpdate { label, .. }
            | Self::Failure { label, .. }
            | Self::Delay { label, .. }
            | Self::Timeout { label, .. }
            | Self::Trigger { label, .. }
            | Self::SubWorkflow { label, .. } => label,
        }
    }

    /// Snake-case wire tag. The registry's `NodeDecl::wire_name` is the
    /// single source of truth — this method is a thin lookup for callers
    /// that have `&WorkflowNodeData` but no `NodeDecl` in scope.
    pub fn type_name(&self) -> &'static str {
        crate::nodes::lookup_by_variant(self)
            .expect("every WorkflowNodeData variant is registered in crate::nodes::NODES")
            .wire_name
    }

    pub fn description(&self) -> Option<&str> {
        match self {
            Self::Start { description, .. }
            | Self::End { description, .. }
            | Self::HumanTask { description, .. }
            | Self::AutomatedStep { description, .. }
            | Self::Agent { description, .. }
            | Self::Decision { description, .. }
            | Self::ParallelSplit { description, .. }
            | Self::Join { description, .. }
            | Self::Loop { description, .. }
            | Self::Scope { description, .. }
            | Self::LeaseScope { description, .. }
            | Self::Map { description, .. }
            | Self::StreamFold { description, .. }
            | Self::PhaseUpdate { description, .. }
            | Self::ProgressUpdate { description, .. }
            | Self::Failure { description, .. }
            | Self::Delay { description, .. }
            | Self::Timeout { description, .. }
            | Self::Trigger { description, .. }
            | Self::SubWorkflow { description, .. } => description.as_deref(),
        }
    }

    /// Typed input ports declared or derived for this variant. Routes to
    /// the registry's per-variant `input_ports` fn pointer; the actual
    /// derivation lives in `service/src/nodes/<variant>.rs`.
    ///
    /// An empty list means "single anonymous input" — edges with
    /// `target_handle: "in"` still resolve via the pass-through path in
    /// `validate_edges_typed`.
    pub fn input_ports(&self) -> Vec<Port> {
        let decl = crate::nodes::lookup_by_variant(self)
            .expect("every WorkflowNodeData variant is registered in crate::nodes::NODES");
        (decl.input_ports)(self)
    }

    /// Typed output ports declared or derived for this variant. Routes to
    /// the registry's per-variant `output_ports` fn pointer; the actual
    /// derivation lives in `service/src/nodes/<variant>.rs`.
    ///
    /// Derived-shape variants worth knowing (the derivation lives in the
    /// per-variant module): `HumanTask` unions step input fields; `Decision`
    /// emits one port per branch + the default; `Agent`/`AutomatedStep`/
    /// `SubWorkflow` append a canonical `error` port; `Loop` exposes outer
    /// `out` plus `body_in`; `End` returns empty.
    pub fn output_ports(&self) -> Vec<Port> {
        let decl = crate::nodes::lookup_by_variant(self)
            .expect("every WorkflowNodeData variant is registered in crate::nodes::NODES");
        (decl.output_ports)(self)
    }
}

/// Derive a HumanTask's output port from the union of Input fields across all
/// task steps. Duplicate field names (first-wins) and non-input blocks are
/// silently ignored — the editor enforces uniqueness during authoring, and
/// the human-task form UI is the source of truth for behavior.
pub(crate) fn derive_human_task_output_port(steps: &[TaskStepConfig]) -> Port {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut fields: Vec<PortField> = Vec::new();
    for step in steps {
        for block in &step.blocks {
            if let TaskBlockConfig::Input { field } = block {
                if seen.insert(field.name.clone()) {
                    fields.push(PortField {
                        schema: None,
                        name: field.name.clone(),
                        label: field.label.clone(),
                        kind: FieldKind::from(field.kind),
                        required: field.required.unwrap_or(false),
                        options: field.options.clone(),
                        description: None,
                        accept: None,
                    });
                }
            }
        }
    }
    Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields,
    }
}

/// Single-source JSON Schema for a dynamic-form step list — `Vec<TaskStepConfig>`,
/// the runtime shape an upstream producer (agent or script) must emit to drive a
/// HumanTask whose `stepsRef` points at it.
///
/// This ONE artifact is reused so the two consumers can't drift:
///   1. **Advertised** to an agent — surfaced on the HumanTask input port (via
///      `PortField::schema`) and thence into the agent tool's `input_schema`, so
///      the model is told the exact block grammar to produce.
///   2. **Enforced** at runtime — emitted as a colored-token `definition` bound
///      to the place the produced blocks land in, so the engine's
///      `SchemaRegistry` validates them like any other typed token.
///
/// Generated from the `#[derive(schemars::JsonSchema)]` on `TaskStepConfig` /
/// `TaskBlockConfig` / `TaskFieldConfig` and friends — the Rust types are the
/// single source of truth, mirroring the `ResourceType` schema precedent in
/// `shared/resources`.
pub fn task_step_list_json_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(Vec<TaskStepConfig>);
    serde_json::to_value(schema).expect("TaskStepConfig JsonSchema must serialize cleanly")
}

impl From<TaskFieldKind> for FieldKind {
    fn from(k: TaskFieldKind) -> Self {
        match k {
            TaskFieldKind::Text => FieldKind::Text,
            TaskFieldKind::Textarea => FieldKind::Textarea,
            TaskFieldKind::Number => FieldKind::Number,
            TaskFieldKind::Select => FieldKind::Select,
            // Checkbox → Bool (the typed-ports superset). HumanTask form UI
            // still renders a checkbox; the wire kind on the derived port is
            // a proper Bool so guards can use `step.flag == true`.
            TaskFieldKind::Checkbox => FieldKind::Bool,
            TaskFieldKind::File => FieldKind::File,
            TaskFieldKind::Signature => FieldKind::Signature,
            // Radio is a Select with inline option rendering — wire kind is
            // identical so downstream borrow-checking treats them the same.
            TaskFieldKind::Radio => FieldKind::Select,
            // Date is an ISO 8601 string on the wire; reuse Text so guards
            // can do lexicographic comparison (`step.due < "2026-01-01"`).
            // A dedicated `FieldKind::Date` could come later if we want
            // typed-date guard helpers; for now Text-with-format is enough.
            TaskFieldKind::Date => FieldKind::Text,
            // Range / Rating both emit numbers; min/max/step/max_rating are
            // renderer hints, not wire-shape constraints.
            TaskFieldKind::Range => FieldKind::Number,
            TaskFieldKind::Rating => FieldKind::Number,
        }
    }
}

// --- Task step configuration (maps to human-ui TaskStep) ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskStepConfig {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_mdsvex: Option<String>,
    pub blocks: Vec<TaskBlockConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
#[serde(tag = "type")]
pub enum TaskBlockConfig {
    #[serde(rename = "input")]
    Input { field: TaskFieldConfig },
    #[serde(rename = "mdsvex")]
    Mdsvex { content: String },
    #[serde(rename = "callout")]
    Callout {
        severity: CalloutSeverity,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        content: String,
    },
    #[serde(rename = "divider")]
    Divider,
    /// `filenames` references compile-time staged assets; `url` is a direct
    /// (often `{{ <slug>.<field> }}`-interpolated) source resolved at instance
    /// time. When `url` is set the human-task UI renders it as the image
    /// source (matching the frontend `{type:"image",url,alt?,caption?}`).
    #[serde(rename = "image")]
    Image {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        filenames: Vec<String>,
        #[serde(default)]
        display: ImageDisplay,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    #[serde(rename = "file")]
    File { filename: String },
    /// Embedded PDF viewer (rendered inline in the task UI). `height` is a
    /// CSS length string, default ~"400px"; `caption` is rendered above the
    /// viewer. `url`, when set (typically via `{{ <slug>.<field> }}`
    /// interpolation), is the direct PDF source.
    #[serde(rename = "pdf")]
    Pdf {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filename: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<String>,
    },
    /// Downloadable artifact list. Serializes to the frontend
    /// `{type:"download",downloads:[{url,filename,...}]}` shape. `url` is
    /// typically `{{ <slug>.<field> }}`-interpolated to an uploaded file.
    #[serde(rename = "download")]
    Download { downloads: Vec<DownloadItemConfig> },
    /// Feature B — render N copies of a sub-task body, one per element of an
    /// upstream array. `items_ref` is a Feature-B `<slug>.<field>[*]`
    /// reference; the compiler synthesizes a read-arc on the parked array
    /// and the frontend renderer iterates `task.data[<items_ref>]`,
    /// instantiating the sub-`blocks` per element. The block's typed
    /// output is `<output_slug>.results: array<{<inputs>}>`, where
    /// `<inputs>` is the union of every `Input` child block's field —
    /// visible to downstream pickers via the standard `TyDescriptor::Array`
    /// machinery. Non-Input children (Mdsvex, Callout, Divider, Image,
    /// Pdf, File, Download) are render-only and contribute nothing to the
    /// per-row schema.
    ///
    /// `item_label_ref`, when set, names a `<slug>.<field>[*].<label>`
    /// ref whose per-element string is used as the row header (e.g. the
    /// task title from an LLM-extracted task list). Static-only: B v1
    /// rejects `[*]` chained twice (`NestedIterationUnsupported`) and
    /// rejects a Repeater nested inside another Repeater.
    #[serde(rename = "repeater")]
    Repeater {
        /// Producer-namespaced ref carrying exactly one `[*]` boundary,
        /// e.g. `extract.tasks[*]`. The pre-`[*]` segments address an
        /// upstream parked array; iteration happens consumer-side.
        items_ref: String,
        /// Optional per-element row label ref, e.g.
        /// `extract.tasks[*].title`. Must share the same iteration prefix
        /// as `items_ref`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_label_ref: Option<String>,
        /// The sub-task body rendered per element. Any TaskBlockConfig
        /// variant except a nested Repeater. `Input` children declare the
        /// per-row form schema and contribute to `<output_slug>.results`
        /// element shape; display blocks (Mdsvex/Callout/Image/Pdf/File/
        /// Download/Divider) are rendered per row with placeholders
        /// resolved against the current row's element.
        ///
        /// `no_recursion` breaks the recursive schema cycle for
        /// utoipa — the wire schema still references `TaskBlockConfig`
        /// via `$ref`, but the generator stops descending here.
        #[schema(no_recursion)]
        blocks: Vec<TaskBlockConfig>,
        /// Rhai-safe slug under which the Repeater's typed output is
        /// addressable downstream as `<output_slug>.results`. Defaults to
        /// the parent HumanTask's slug when empty; must be unique within
        /// the graph (the compiler's existing slug-collision check
        /// covers it).
        output_slug: String,
    },
}

/// One entry in a `download` task block. Mirrors the frontend `DownloadItem`
/// (`app/src/lib/hpi/types.ts`) field-for-field on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct DownloadItemConfig {
    pub url: String,
    pub filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Severity for callout blocks. Snake-case on the wire (`"info"`,
/// `"warning"`, `"error"`, `"success"`) to keep the byte-for-byte shape that
/// the editor and human-task UI already produce/consume.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CalloutSeverity {
    Info,
    Warning,
    Error,
    Success,
}

/// Layout mode for image blocks. Snake-case wire values: `"single"`,
/// `"grid"`, `"gallery"`.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    ToSchema,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ImageDisplay {
    #[default]
    Single,
    Grid,
    Gallery,
}

/// How a `Join { mode: All }` merges the tokens arriving on its joined
/// branches.
///
/// `ShallowLastWins` is the historical behaviour (top-level keys overwrite,
/// last branch to arrive wins on a key collision). `DeepMerge` recursively
/// merges nested object values instead of overwriting them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    #[default]
    ShallowLastWins,
    DeepMerge,
}

/// Firing rule for a `Join` node. `All` (the default) is the AND-join —
/// waits for every incoming branch. `Any` fires per arriving token — the
/// canonical petri-net XOR-join, dual of `Decision`'s XOR-split.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JoinMode {
    #[default]
    All,
    Any,
}

/// Author-selected status for a `PhaseUpdate` control node. Serialized
/// snake_case so it lands on the breadcrumb exactly as the executor
/// `PhaseStatus` the causality consumer deserializes in `record_phase_event`
/// (`aithericon_executor_domain::PhaseStatus`). `Pending` is intentionally
/// omitted — an author explicitly marking a phase always means it is at least
/// running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PhaseUpdateStatus {
    #[default]
    Running,
    Completed,
    Failed,
    Skipped,
}

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

/// Which concrete child-template version a `SubWorkflow` embeds. Internally
/// tagged on the wire: `{"mode":"latest"}` or `{"mode":"pinned","version":3}`.
/// `Latest` is an *authoring* intent only — it is resolved to a concrete
/// version at the parent's publish time and the resolved AIR is frozen into
/// the parent, so runtime is always deterministic / replay-safe. Keep the
/// `mode` strings in lockstep with the `snake_case` derive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum VersionPin {
    /// Resolve the family's `is_latest` row at parent publish time.
    #[default]
    Latest,
    /// Freeze an explicit child version.
    Pinned { version: i32 },
}

/// Deserialization default for `SubWorkflow.output` — an empty `out` port
/// (Json pass-through of the child's terminal result). Authoring can prefill
/// fields from the child's End `terminal` port.
pub fn default_subworkflow_output_port() -> Port {
    Port {
        id: "out".to_string(),
        label: "Result".to_string(),
        fields: vec![],
    }
}

/// Deserialization default for `SubWorkflow.input_contract` — an empty `in`
/// port. Display-only; the real contract is filled by publish reconcile / the
/// editor's io-contract fetch. Existing graphs without the field load unchanged.
pub fn default_subworkflow_input_contract() -> Port {
    Port::empty_input()
}

/// Deserialization default for `Join.output` — an empty `out` port. The
/// editor or author fills in the fields the join exposes downstream via
/// `<slug>.<field>`.
pub fn default_join_output_port() -> Port {
    Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields: vec![],
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
        #[serde(rename = "jobTemplateRef", default, skip_serializing_if = "Option::is_none")]
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

/// A binding to a `datacenter` resource for a loop-scoped lease (L3). Lives
/// under [`WorkflowNodeData::Loop`]'s `lease`; its presence makes `lower_loop`
/// hoist the claim/grant/register/release handshake to loop scope — ONE
/// allocation held across all iterations, released exactly once on exit.
///
/// Mirrors [`DeploymentModel::Scheduled`]'s `scheduler: Option<String>` +
/// `request: Option<Value>` and [`CapacityBinding`] so the existing
/// `resolve_binding(..., "datacenter", ...)` + lease-definition machinery
/// applies unchanged. The field is named `scheduler` (not `alias`) for symmetry
/// with the `Scheduled` lease path the loop body would otherwise inherit
/// per-step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaseBinding {
    /// `datacenter` resource alias (workspace alias) the loop holds a lease
    /// against. Resolved at publish to `pool-<resource_id>` + the
    /// `Lease__scheduler` schema, the same path as `Scheduled.scheduler`
    /// (`resolve_binding(.., SchedulerLease, ..)`).
    pub scheduler: String,
    /// Claim-schema-shaped request params (`gpu_count`/`gpu_type`/
    /// `max_duration_secs`); validated against the datacenter kind's
    /// `claim_schema`. `None` ⇒ the allocator's default placement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<serde_json::Value>,
}

/// LLM model + provider selection for an [`WorkflowNodeData::Agent`]. Mirrors
/// the subset of `aithericon_executor_backend_configs::llm::LlmConfig` the
/// editor authors directly (provider, model, optional creds / sampling
/// knobs); the degenerate single-turn lowering reconstructs the full
/// `LlmConfig` from these fields plus the Agent's prompts. Wire shape
/// matches the existing `LlmConfig` JSON one-for-one so the equivalence
/// test (PR 1) produces byte-identical `config_ref` blobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelRef {
    /// `"openai"` | `"anthropic"` | `"ollama"`. Wire format is lowercase to
    /// line up with `LlmConfig::Provider`'s `rename_all = "lowercase"`.
    pub provider: String,
    /// Provider-specific model identifier (e.g. `"gpt-4o"`,
    /// `"claude-sonnet-4-20250514"`).
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Workspace resource alias the LLM call binds to (e.g. `"openai_prod"`).
    /// Same channel as `LlmConfig::resource_alias` — the compiler emits a
    /// `ResourceEnvelope` borrow when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_alias: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

/// Context-window management strategy for an [`WorkflowNodeData::Agent`].
/// Inert in PR 1's degenerate path; declared upfront so the type stays
/// stable across the follow-up loop-lowering PR (`docs/12` § 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContextStrategy {
    #[default]
    None,
    /// Drop oldest non-system messages once the budget is exceeded.
    DropOldest,
    /// Summarize oldest messages into a single rolling summary turn.
    SummarizeOldest,
}

/// What happens when a tool call inside an [`WorkflowNodeData::Agent`]
/// fails after the tool's own retry budget is exhausted. Default `Feedback`
/// — append a synthetic `role: tool, content: "Tool '<name>' failed: …"`
/// message to the conversation and re-enter the LLM call. `Bubble` routes
/// the failure straight to the agent's `error` output. Inert in PR 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorPolicy {
    #[default]
    Feedback,
    Bubble,
}

fn default_max_turns() -> u32 {
    1
}

/// Default `Map.item_var` — body tokens bind the per-element value as `item`.
fn default_item_var() -> String {
    "item".to_string()
}

/// Default `StreamFold.result_var` — chunks bind their value as `item`.
fn default_stream_result_var() -> String {
    "item".to_string()
}

/// How a `StreamFold` folds the drained chunks into its single output token.
/// Tagged on `kind` (camelCase), mirroring the serde conventions of the other
/// config enums. Each variant selects the gather barrier's reduce Rhai in
/// `compiler/lower/stream_fold.rs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[derive(Default)]
pub enum StreamReduce {
    /// Ordered array — sort chunks by stream sequence, project each `.value`
    /// into a `Vec`. The default (matches Map's gather reduce).
    #[default]
    Array,
    /// String-join the chunk `.value`s (rendered as strings) in stream order,
    /// optionally separated by `sep`.
    Concat {
        #[serde(skip_serializing_if = "Option::is_none")]
        sep: Option<String>,
    },
    /// Numeric sum of the chunk `.value`s, in stream order.
    Sum,
    /// Author-supplied Rhai over `__r` (the sorted array of
    /// `#{ value, __map_idx, __map_id }`), returning the reduced value.
    Custom { expr: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct TaskFieldConfig {
    pub name: String,
    pub label: String,
    pub kind: TaskFieldKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Per-field helper text shown under the input. Mdsvex source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description_mdsvex: Option<String>,
    /// Choice list for `kind = "select"` / `"radio"`. Authored as
    /// `[{"value": "approve", "label": "Approve"}, …]` — `value` is the
    /// canonical wire value submitted by the form, `label` is the
    /// human-facing display string. A bare string shorthand
    /// (`["approve", "reject"]`) is accepted at deserialize time and
    /// normalized to `{value, label}` where `label = value` — convenient
    /// for trivial sets while keeping the runtime representation uniform.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_task_field_options"
    )]
    pub options: Option<Vec<SelectOption>>,
    /// For `File` kind: accepted file types as an HTML input `accept`
    /// attribute (e.g. `"image/png,image/jpeg,.pdf"`). Ignored for
    /// non-file kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept: Option<String>,
    /// For `File` kind: maximum file size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_file_size: Option<u64>,
    /// For `File` kind: maximum number of files (defaults to 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_files: Option<u32>,
    /// For `Signature` kind: capture mode (currently only `"draw"` is
    /// implemented).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_mode: Option<String>,
    /// For `Signature` kind: ink color (CSS color string).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pen_color: Option<String>,
    /// For `Number` / `Range` kinds: minimum allowed value (inclusive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// For `Number` / `Range` kinds: maximum allowed value (inclusive).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// For `Number` / `Range` kinds: step increment (defaults to 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<f64>,
    /// For `Rating` kind: number of stars (defaults to 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rating: Option<u32>,
    /// For `Date` kind: when true, capture date + time (`YYYY-MM-DDTHH:MM`);
    /// otherwise capture date only (`YYYY-MM-DD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_time: Option<bool>,
}

/// One choice in a `kind = "select"` field. `value` is what the form
/// submits / what guards downstream compare against; `label` is what the
/// UI renders. Authors typically write `{value, label}`; the deserializer
/// also accepts a bare string and stretches it to `{value: s, label: s}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema, schemars::JsonSchema)]
pub struct SelectOption {
    pub value: String,
    pub label: String,
}

/// Hand-rolled deserializer for `TaskFieldConfig::options`. Accepts two
/// authoring shapes and normalizes to `Vec<SelectOption>`:
///
///   - `["approve", "reject"]` — bare string shorthand for the common
///     case where the value doubles as the label. Stretched to
///     `{value: "approve", label: "approve"}` etc.
///   - `[{"value": "approve", "label": "Approve as-extracted"}, …]` —
///     full rich shape.
///
/// Any other shape (numbers, bools, mixed arrays without those exact
/// keys) is rejected with an actionable error that names the field
/// index — much better than serde's default "invalid type" surface that
/// doesn't point at the offending entry.
fn deserialize_task_field_options<'de, D>(de: D) -> Result<Option<Vec<SelectOption>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let value = Option::<serde_json::Value>::deserialize(de)?;
    let Some(value) = value else {
        return Ok(None);
    };
    let arr = value.as_array().ok_or_else(|| {
        D::Error::custom(
            "task field `options` must be a list (either of strings or of `{value,label}` objects)",
        )
    })?;
    let mut out = Vec::with_capacity(arr.len());
    for (i, item) in arr.iter().enumerate() {
        match item {
            serde_json::Value::String(s) => out.push(SelectOption {
                value: s.clone(),
                label: s.clone(),
            }),
            serde_json::Value::Object(map) => {
                let value = map
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        D::Error::custom(format!(
                            "task field `options[{i}]` is an object but missing a string `value` key"
                        ))
                    })?
                    .to_string();
                // `label` is optional — defaults to `value` so trivial
                // entries can be authored as `{"value": "approve"}`.
                let label = map
                    .get("label")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| value.clone());
                out.push(SelectOption { value, label });
            }
            other => {
                return Err(D::Error::custom(format!(
                    "task field `options[{i}]` must be a string or `{{value,label}}` object; got {}",
                    match other {
                        serde_json::Value::Null => "null",
                        serde_json::Value::Bool(_) => "a boolean",
                        serde_json::Value::Number(_) => "a number",
                        serde_json::Value::Array(_) => "a list",
                        _ => "an unsupported value",
                    }
                )));
            }
        }
    }
    Ok(Some(out))
}

/// Form-field control kind for `input` task blocks. Snake-case wire values
/// such as `"text"`, `"textarea"`, `"number"`, `"select"`, `"checkbox"`,
/// `"file"`, `"signature"`, `"radio"`, `"date"`, `"range"`, `"rating"`.
/// Must stay in sync with the engine's `TaskFieldKind` in
/// `engine/core-engine/crates/domain/src/human.rs` and the frontend's
/// `TASK_FIELD_KINDS` in `app/src/lib/hpi/types.ts` — drift means the
/// compiler accepts an author's choice that the engine rejects (or
/// vice-versa).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    Serialize,
    Deserialize,
    ToSchema,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskFieldKind {
    #[default]
    Text,
    Textarea,
    Number,
    Select,
    Checkbox,
    File,
    Signature,
    /// Radio button group — same `{value, label}` options as `Select`,
    /// rendered inline (all options visible at once) instead of as a
    /// dropdown. Picked-value wire shape matches `Select`.
    Radio,
    /// Date picker (`YYYY-MM-DD`) or datetime picker (`YYYY-MM-DDTHH:MM`)
    /// when the field carries `include_time = true`. Wire value is a
    /// plain ISO string; downstream comparisons can use lexicographic
    /// ordering up to minute precision.
    Date,
    /// Slider control — emits a `number` on the wire. Customize the
    /// span via `min` / `max` / `step` on the field config.
    Range,
    /// Star-rating control — emits a `number` from 0 to `max_rating`
    /// (default 5) on the wire.
    Rating,
}

/// Type kind for a typed port field. Superset of `TaskFieldKind`: adds `Bool`
/// (currently piggybacks on `Checkbox` in human-task forms), `Timestamp`
/// (needed for trigger fire times and audit fields), and `Json` (opaque
/// escape hatch for legacy / dynamic payloads). Snake-case wire values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Textarea,
    Number,
    Bool,
    Select,
    File,
    Signature,
    Timestamp,
    Json,
    /// First-class container marker for a nested JSON object. The recursive
    /// SHAPE lives in [`PortField::schema`] (a JSON Schema); this kind only
    /// marks the field as an object container. With a `schema` the emitted
    /// contract is that schema verbatim; without one it falls back to a
    /// permissive `{"type":"object","additionalProperties":true}`.
    Object,
    /// First-class container marker for a nested JSON array. The element/item
    /// SHAPE lives in [`PortField::schema`]; without one it falls back to a
    /// permissive `{"type":"array"}`.
    Array,
}

impl FieldKind {
    /// Best-effort runtime check that a JSON value is acceptable for this kind.
    /// `Json` accepts anything. Used by `parameterize_air` to validate start
    /// tokens against the declared Start `initial` port.
    pub fn accepts(&self, value: &serde_json::Value) -> bool {
        match self {
            Self::Json => true,
            Self::Bool => value.is_boolean(),
            Self::Number => value.is_number(),
            Self::Text | Self::Textarea | Self::Select | Self::Signature | Self::Timestamp => {
                value.is_string()
            }
            // File is a catalog reference (`file_metadata::StoragePath`); accept
            // any string or object, validation happens deeper.
            Self::File => value.is_string() || value.is_object(),
            // Container markers: shallow shape check only — deep validation is
            // deferred to the runtime `SchemaRegistry` via the emitted schema.
            // (Null is tolerated as absent by `validate_token` before we ever
            // get here, so no explicit null arm is needed.)
            Self::Object => value.is_object(),
            Self::Array => value.is_array(),
        }
    }

    /// The bare JSON Schema type for this kind — no field-level enrichment.
    /// This is the single derivation point that keeps `accepts` (runtime
    /// validation) and the emitted contract schema in lockstep: an anti-drift
    /// test asserts they agree per kind.
    pub fn base_schema(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            Self::Text | Self::Textarea | Self::Select | Self::Signature => json!({"type": "string"}),
            Self::Number => json!({"type": "number"}),
            Self::Bool => json!({"type": "boolean"}),
            Self::Timestamp => json!({"type": "string", "format": "date-time"}),
            // File is a storage-path / catalog reference on the wire.
            Self::File => json!({"type": "string"}),
            // Json is the opaque escape hatch — anything goes.
            Self::Json => json!({}),
            // Container markers with no author `schema` override stay permissive:
            // an object accepts any keys, an array any items. A `field.schema`
            // (handled in `json_schema`) replaces these verbatim.
            Self::Object => json!({"type": "object", "additionalProperties": true}),
            Self::Array => json!({"type": "array"}),
        }
    }

    /// Field-aware JSON Schema layered on [`base_schema`]. An explicit author
    /// `field.schema` always wins (returned verbatim). Otherwise the base type
    /// is enriched with a `Select` `enum` from the field's options and the
    /// field `description` when present.
    pub fn json_schema(&self, field: &PortField) -> serde_json::Value {
        if let Some(s) = &field.schema {
            return s.clone();
        }
        let mut schema = self.base_schema();
        if matches!(self, Self::Select) {
            if let Some(options) = &field.options {
                schema["enum"] = serde_json::Value::Array(
                    options
                        .iter()
                        .map(|o| serde_json::Value::String(o.value.clone()))
                        .collect(),
                );
            }
        }
        if let Some(desc) = &field.description {
            schema["description"] = serde_json::Value::String(desc.clone());
        }
        schema
    }
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

/// A single field within a typed `Port`. Identifier-like `name` is the wire
/// key in the token; `label` is for display.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortField {
    pub name: String,
    pub label: String,
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    /// Choice list for `kind = Select`. Same `{value, label}` shape as
    /// [`TaskFieldConfig::options`]; the deserializer accepts either bare
    /// strings or `{value, label}` objects and normalizes to the rich
    /// form. See [`SelectOption`].
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_task_field_options"
    )]
    pub options: Option<Vec<SelectOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// For `File` kind: accepted formats as an HTML input `accept` list
    /// (comma-separated MIME types and/or extensions, e.g.
    /// `"image/png,image/jpeg,.pdf"`). The instance-launch upload widget
    /// uses this to filter the picker, reject mismatched files, and show
    /// the expected formats. Ignored for non-file kinds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept: Option<String>,
    /// Optional rich JSON Schema override for this field's value, for the cases
    /// where the flat `kind` vocabulary (`Json` being the only escape hatch)
    /// can't express the real structure. When present it is the field's
    /// authoritative schema everywhere a richer-than-scalar shape is needed:
    /// it becomes the agent-tool `input_schema` property (so a model calling a
    /// node as a tool is told the exact nested shape to produce), and it feeds
    /// the colored-token definition the runtime `SchemaRegistry` enforces. The
    /// canonical use is the dynamic-form HumanTask: its `steps` input field
    /// carries [`task_step_list_json_schema`] so the agent and the runtime
    /// share one single-sourced `TaskStepConfig[]` contract. Not author-facing
    /// — derived by node lowering, never hand-edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
}

/// A named bundle of typed fields exchanged at a block boundary. Two ports
/// type-match if their field sets are equal (same names, same kinds, with
/// `Json` as the escape hatch).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Port {
    /// Unique within the block (e.g. `"in"`, `"out"`, `"approved"`).
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub fields: Vec<PortField>,
}

impl Port {
    /// JSON Schema for this port as an object contract. An empty (undeclared)
    /// port stays permissive — `additionalProperties: true`, no locked shape —
    /// rather than collapsing to `{}` which would also accept non-objects.
    /// A declared port is `additionalProperties: false` with per-field
    /// properties (via [`FieldKind::json_schema`]) and a `required` list built
    /// from the required fields (omitted entirely when none are required).
    pub fn json_schema(&self) -> serde_json::Value {
        use serde_json::json;
        if self.fields.is_empty() {
            return json!({"type": "object", "additionalProperties": true});
        }
        let properties: serde_json::Map<String, serde_json::Value> = self
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.kind.json_schema(f)))
            .collect();
        let required: Vec<serde_json::Value> = self
            .fields
            .iter()
            .filter(|f| f.required)
            .map(|f| serde_json::Value::String(f.name.clone()))
            .collect();
        let mut schema = json!({
            "type": "object",
            "properties": properties,
            "additionalProperties": false,
        });
        if !required.is_empty() {
            schema["required"] = serde_json::Value::Array(required);
        }
        schema
    }
}

/// One fold/scan slot on a [`WorkflowNodeData::Loop`]. Lives as an additional
/// field in the loop's parked `p_<id>_data` envelope (the iteration counter
/// generalized): `init` is evaluated once in the enter transition, `merge_expr`
/// is re-evaluated write-once-per-iteration in the continue transition. Both
/// are Rhai expressions. `merge_expr` may reference the prior accumulator value
/// as `<loop_slug>.<var>` and the body's output as `<body_slug>.<field>` — the
/// standard read-arc synthesis resolves those borrows automatically.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoopAccumulator {
    /// Rhai identifier the accumulator is addressed by, both inside the loop's
    /// own continue transition (`<loop_slug>.<var>`) and downstream. Must not
    /// be `iteration` (reserved) and must be unique within the loop.
    pub var: String,
    /// Rhai expression evaluated in the enter transition scope (the entering
    /// workflow token is bound as `input`). Keep simple — e.g. `"0"`, `"[]"`,
    /// `"#{}"`.
    pub init: String,
    /// Rhai expression evaluated in the continue transition scope, producing the
    /// next accumulator value. References the prior value as `<loop_slug>.<var>`
    /// and body output as `<body_slug>.<field>`.
    #[serde(rename = "mergeExpr")]
    pub merge_expr: String,
}

impl Port {
    /// Empty input port — used as the deserialization default for `Start.initial`
    /// and similar so existing templates load unchanged.
    pub fn empty_input() -> Self {
        Self {
            id: "in".to_string(),
            label: "Input".to_string(),
            fields: vec![],
        }
    }

    /// Validate a candidate token against this port's declared fields.
    ///
    /// Validation only — never coerces. `Json`/`File` kinds are permissive
    /// escape hatches (see [`FieldKind::accepts`]). A port with no `fields`
    /// accepts any object (pass-through ports). This is the *single* rule
    /// enforced for every token entering any port: a Start block's `initial`
    /// port (via `petri::instance::parameterize_air`) and in-flight signal
    /// ports (via the trigger dispatcher's signal path). Keeping one
    /// implementation guarantees the spawn and signal paths can't diverge.
    pub fn validate_token(&self, token: &serde_json::Value) -> Result<(), PortValidationError> {
        let obj = token.as_object().ok_or(PortValidationError::NotObject)?;
        for field in &self.fields {
            match obj.get(&field.name) {
                None if field.required => {
                    return Err(PortValidationError::MissingRequiredField {
                        field: field.name.clone(),
                    });
                }
                None => {} // optional and absent — fine
                Some(v) if v.is_null() && field.required => {
                    return Err(PortValidationError::MissingRequiredField {
                        field: field.name.clone(),
                    });
                }
                Some(v) if v.is_null() => {} // optional null — fine
                Some(v) if !field.kind.accepts(v) => {
                    return Err(PortValidationError::FieldKindMismatch {
                        field: field.name.clone(),
                        kind: field.kind,
                    });
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

/// Why a token failed [`Port::validate_token`]. Context-free by design — the
/// caller adds the block / trigger identity (`parameterize_air` maps these into
/// its `ParameterizeError`; the dispatcher maps them into a dropped-fire
/// reason).
#[derive(Debug, thiserror::Error)]
pub enum PortValidationError {
    /// Token isn't a JSON object — every port is field-keyed.
    #[error("token must be a JSON object")]
    NotObject,
    /// A required field is absent (or explicitly null).
    #[error("field '{field}' is required but missing")]
    MissingRequiredField { field: String },
    /// A field is present but its JSON kind doesn't match the declared
    /// `FieldKind` (e.g. a string supplied for a `Number` field).
    #[error("field '{field}' has wrong type for kind {kind:?}")]
    FieldKindMismatch { field: String, kind: FieldKind },
}

pub fn default_initial_port() -> Port {
    Port::empty_input()
}

pub fn default_terminal_port() -> Port {
    Port {
        id: "in".to_string(),
        label: "Terminal".to_string(),
        fields: vec![],
    }
}

pub fn default_automated_input_port() -> Port {
    Port::empty_input()
}

/// Canonical output-port shape for an `AutomatedStep` whose `output` field
/// hasn't been customized. Each backend declares the fields its executor
/// reliably surfaces via [`crate::backends::BackendDecl::default_output_fields`].
/// Editor exposes "Reset to default" by re-deriving against the current
/// `backendType`.
///
/// `BACKENDS` covers every `ExecutionBackendType` variant — the registry test
/// (`backend_registry_coverage.rs`) enforces it. No fallback needed.
pub fn default_output_port(backend: ExecutionBackendType) -> Port {
    let decl = crate::backends::lookup(backend)
        .expect("BACKENDS must cover every ExecutionBackendType; enforced by registry test");
    Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields: decl
            .default_output_fields
            .iter()
            .map(|f| f.into_port_field())
            .collect(),
    }
}

/// Agent-specific fields the loop path packs under `detail.outputs`
/// alongside the canonical Llm four. Declared so the picker surfaces
/// `<agent_slug>.turn`, `<agent_slug>.history`, etc. without the author
/// having to know they exist. Source of truth for what `t_*_route_final`
/// emits in `service/src/compiler/lower/agent.rs`.
pub(crate) fn agent_extra_output_fields() -> Vec<PortField> {
    vec![
        PortField {
            schema: None,
            name: "turn".to_string(),
            label: "Final turn count".to_string(),
            kind: FieldKind::Number,
            required: false,
            options: None,
            description: Some("Number of LLM round-trips before the agent exited.".to_string()),
            accept: None,
        },
        PortField {
            schema: None,
            name: "history_ref".to_string(),
            label: "Conversation transcript blob".to_string(),
            kind: FieldKind::Text,
            required: false,
            options: None,
            description: Some(
                "Storage key of the final cumulative transcript blob (the full \
                 `{role, content, …}` conversation lives off-token in object storage)."
                    .to_string(),
            ),
            accept: None,
        },
        PortField {
            schema: None,
            name: "final_response".to_string(),
            label: "Full LLM turn result".to_string(),
            kind: FieldKind::Json,
            required: false,
            options: None,
            description: Some(
                "The last `LlmTurnResult` (content, tool_calls, stop_reason, usage).".to_string(),
            ),
            accept: None,
        },
        PortField {
            schema: None,
            name: "input".to_string(),
            label: "Original input".to_string(),
            kind: FieldKind::Json,
            required: false,
            options: None,
            description: Some("The inbound token the agent received.".to_string()),
            accept: None,
        },
    ]
}

pub fn default_automated_output_port() -> Port {
    // Serde default fires before we know the backend type, so we fall back to a
    // generic empty port. `AutomatedStep::ensure_output_default` (called from
    // the compiler and editor) re-derives backend-specific fields when the
    // stored `output` is the bare default and the user hasn't customized it.
    Port {
        id: "out".to_string(),
        label: "Output".to_string(),
        fields: vec![],
    }
}

/// Single source of truth for "what LLM config would this Agent send?".
/// The agent loop, the degenerate-path delegate, the resource borrow
/// planner, the publish-time resource-discovery scan, and the
/// `output_ports` deriver all need an equivalent `LlmConfig` payload —
/// before this helper they each rebuilt their own subset and drifted.
///
/// Field names match `aithericon_executor_backend_configs::llm::LlmConfig`
/// 1:1 so `validate_and_transform`'s LLM arm round-trips this without
/// coercion. `tools` is passed through verbatim — the agent loop populates
/// it with one entry per tool child; resource discovery / borrow planning
/// pass `&[]` because tool wiring is irrelevant to those scans.
pub fn agent_to_llm_config(
    model: &ModelRef,
    system_prompt: Option<&str>,
    user_prompt: &str,
    response_format: Option<&serde_json::Value>,
    images: &[serde_json::Value],
    tools: &[serde_json::Value],
) -> serde_json::Value {
    use serde_json::{Number, Value};
    let mut config = serde_json::Map::new();
    config.insert(
        "provider".to_string(),
        Value::String(model.provider.clone()),
    );
    config.insert("model".to_string(), Value::String(model.model.clone()));
    if let Some(k) = &model.api_key {
        config.insert("api_key".to_string(), Value::String(k.clone()));
    }
    if let Some(b) = &model.base_url {
        config.insert("base_url".to_string(), Value::String(b.clone()));
    }
    if let Some(a) = &model.resource_alias {
        config.insert("resource_alias".to_string(), Value::String(a.clone()));
    }
    config.insert("prompt".to_string(), Value::String(user_prompt.to_string()));
    if let Some(sp) = system_prompt {
        config.insert("system_prompt".to_string(), Value::String(sp.to_string()));
    }
    if let Some(t) = model.temperature {
        config.insert(
            "temperature".to_string(),
            Number::from_f64(t)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
    }
    if let Some(m) = model.max_tokens {
        config.insert("max_tokens".to_string(), Value::Number(m.into()));
    }
    if let Some(rf) = response_format {
        config.insert("response_format".to_string(), rf.clone());
    }
    if !images.is_empty() {
        config.insert("images".to_string(), Value::Array(images.to_vec()));
    }
    if !tools.is_empty() {
        config.insert("tools".to_string(), Value::Array(tools.to_vec()));
    }
    Value::Object(config)
}

// --- Trigger nodes (Phase 5) ---

/// What event source fires a `Trigger` node. Tagged enum on the wire
/// (`{"kind": "cron", ...}`). Phase 5a only wires `Manual` into the dispatcher
/// end-to-end; the other variants are stored as data and surfaced through the
/// API for the editor to round-trip, but firing logic for each lands in 5b–5e.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TriggerSource {
    Cron(CronTrigger),
    Catalog(CatalogTrigger),
    NetCompletion(NetCompletionTrigger),
    Webhook(WebhookTrigger),
    Manual(ManualTrigger),
}

impl TriggerSource {
    /// Discriminant string used for routing in the dispatcher and metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Cron(_) => "cron",
            Self::Catalog(_) => "catalog",
            Self::NetCompletion(_) => "net_completion",
            Self::Webhook(_) => "webhook",
            Self::Manual(_) => "manual",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CronTrigger {
    /// Standard cron expression (5- or 6-field). Validated at compile time.
    pub schedule: String,
    /// IANA timezone (e.g. `"Europe/Berlin"`). Defaults to `"UTC"` if absent.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Optional jitter window in seconds; the dispatcher fires within
    /// `[scheduled, scheduled + jitter_secs]` to spread load.
    #[serde(default)]
    pub jitter_secs: u32,
    /// What to do after a service restart with missed fire windows.
    #[serde(default)]
    pub catchup: CronCatchup,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum CronCatchup {
    /// Fire every missed window from the last-fire timestamp.
    FireMissed,
    /// Discard missed windows; only fire the next one.
    #[default]
    SkipMissed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogTrigger {
    /// Same filter shape as `CatalogueSubscription.filters`.
    #[serde(default)]
    pub filters: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    /// If true, the dispatcher walks existing catalogue entries matching the
    /// filters when the trigger is first registered.
    #[serde(default)]
    pub backfill: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NetCompletionTrigger {
    /// Source template whose instance completion fires this trigger.
    pub source_template_id: Uuid,
    /// Specific version, or `None` for any published version.
    #[serde(default)]
    pub source_version: Option<i32>,
    /// Which terminal status counts as a fire.
    pub on: CompletionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompletionStatus {
    Success,
    Failure,
    Cancelled,
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebhookTrigger {
    /// Stable slug appended to `/api/triggers/webhook/{slug}`. Must be unique
    /// across published templates — the editor reserves it at publish.
    pub slug: String,
    pub auth: WebhookAuth,
    #[serde(default)]
    pub require_method: Option<HttpMethod>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WebhookAuth {
    /// No auth — endpoint is publicly fireable. Sane only for staging or
    /// trusted networks; the editor surfaces a warning.
    None,
    /// Compare a header (typically `Authorization` or `X-Webhook-Token`) to a
    /// static shared secret. Secret is stored encrypted at rest.
    SharedSecret { header: String, secret_ref: String },
    /// HMAC-SHA256 signature over the request body, with the signing key
    /// stored encrypted at rest and the signature read from `header`.
    SignedHmac { header: String, secret_ref: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ManualTrigger {
    /// Form schema for the "Run with parameters" dialog. Reuses the same
    /// `TaskFieldConfig` shape as human-task forms.
    #[serde(default)]
    pub form: Vec<TaskFieldConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyPolicy {
    /// Every fire produces an event (default).
    #[default]
    Allow,
    /// At most one fire in flight; subsequent fires are dropped while running.
    Skip,
    /// Queue fires behind the active one; drained when it completes.
    Queue,
    /// Dedup by hashing the result of a Rhai `expression` over the event scope;
    /// fires whose key has been seen within `window_secs` are dropped.
    DedupKey {
        expression: String,
        window_secs: u32,
    },
}

/// A single field mapping for `Trigger.payload_mapping`. Each entry projects
/// an event scope into a typed token field via a Rhai expression. The compiler
/// validates that `target_field` exists in the resolved target port and that
/// `expression` parses.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FieldMapping {
    /// Name of the target port field this mapping fills.
    pub target_field: String,
    /// Rhai expression evaluated against the trigger source's event scope
    /// (`payload`, `fire_time`, etc. — varies by source kind).
    pub expression: String,
}

// --- Branch conditions ---

/// xyflow Handle id for a Decision node's otherwise/else branch. The editor's
/// `DecisionNode.svelte` hardcodes this literal as the source-handle id for
/// the Otherwise row, and the compiler's default output place uses the same
/// literal — so an edge with `sourceHandle = "default"` is the only wiring
/// shape that renders correctly in the editor and lowers correctly in the
/// compiler. `WorkflowNodeData::Decision::default_branch` stays
/// `Option<String>` for forward-compat with future multi-default-branch
/// decisions, but `compiler::validate` rejects any other value today.
pub const DEFAULT_BRANCH_HANDLE_ID: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BranchCondition {
    pub edge_id: String,
    pub label: String,
    pub guard: String,
}

// --- Execution spec configuration ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSpecConfig {
    pub backend_type: ExecutionBackendType,
    /// Filename of the entrypoint script within the node's staged files.
    /// Backends that don't run a user script (e.g. `http`) ignore this; the
    /// editor still surfaces it for python/process/docker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    pub config: serde_json::Value,
}

// Wire-tag enum lives in the cross-crate `aithericon-backends` crate so
// `mekhan-service` and `aithericon-executor-service` share one source of
// truth. Re-exported here so existing imports (`models::template::ExecutionBackendType`)
// keep working.
pub use aithericon_backends::ExecutionBackendType;

// --- Edge types ---

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_handle: Option<String>,
    /// Required at publish time (Phase 2 typed-ports). Stays optional in
    /// serde so pre-typed-ports edges still deserialize, but the compiler
    /// rejects `None` with `CompileError::MissingTargetHandle` so existing
    /// templates need an editor pass before they republish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(rename = "type")]
    pub edge_type: String,
}

// --- API request/response types ---

/// Request body for stateless compilation. Used by `POST /api/v1/compile` and
/// `POST /api/v1/templates/{id}/compile`. `files` is a per-node, per-filename map
/// of inline contents; the preview compile emits `InputSource::Raw` entries so
/// the AIR matches the StoragePath-keyed shape produced by publish.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CompileRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub graph: WorkflowGraph,
    #[serde(default)]
    pub files: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    /// Workspace the draft belongs to. When present, `POST /api/v1/analyze`
    /// resolves workspace-scoped **resources** referenced by the graph so the
    /// editor picker / diagnostics see resource public fields (`<resource>.<field>`)
    /// as a known "Globals" scope instead of a false unresolved. Absent on the
    /// stateless `/api/v1/compile` path (which has no DB context).
    #[serde(default)]
    pub workspace_id: Option<uuid::Uuid>,
    /// Template the draft belongs to. When present, `/api/v1/analyze` resolves
    /// template-visible **assets** referenced by the graph (`<asset>.<field>`)
    /// into the same "Globals" scope.
    #[serde(default)]
    pub template_id: Option<uuid::Uuid>,
}

/// Git provenance recorded on a version published via `mekhan apply`.
/// Serialized into the `workflow_templates.source_ref` JSONB column.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SourceRef {
    /// Git remote URL (`git remote get-url origin`).
    pub remote: String,
    /// Commit SHA the artifact was applied from (`git rev-parse HEAD`).
    pub sha: String,
    /// Working tree had uncommitted changes at apply time
    /// (`git status --porcelain` non-empty).
    pub dirty: bool,
    /// Branch / ref name, when resolvable (`git rev-parse --abbrev-ref HEAD`).
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
}

/// Request body for `POST /api/v1/templates/{id}/apply` — the GitOps path.
/// The `graph` REPLACES the chain head wholesale (no CRDT merge); binary
/// assets are uploaded out-of-band via the files endpoint before this call.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ApplyTemplateRequest {
    pub graph: WorkflowGraph,
    #[serde(default)]
    pub files: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
    #[serde(default)]
    pub source_ref: Option<SourceRef>,
}

/// Trigger spec embedded in a `POST /api/templates/apply-air` request.
/// The endpoint synthesizes a `WorkflowGraph` stub containing only this
/// Trigger node so that `register_triggers` (which walks `template.graph`)
/// finds it post-commit. Direct AIR-place binding via
/// `air_target_place_id` — no graph edge.
#[derive(Debug, Deserialize, ToSchema)]
pub struct PreAirTriggerSpec {
    /// Stable, globally-unique node id used in `POST /api/triggers/{node_id}/fire`
    /// URLs. Author-controlled (e.g. `"trg_di_extraction_v1"`).
    pub node_id: String,
    pub label: String,
    /// Trigger source. Clinic's initial use case is `Manual`; other sources
    /// are valid here too (Webhook, Cron, ...) — the dispatcher resolves
    /// them identically once the trigger is registered.
    pub source: TriggerSource,
    #[serde(default)]
    pub payload_mapping: Vec<FieldMapping>,
    /// The AIR place id whose `initial_tokens` will be seeded with the
    /// fire payload + system fields. Must exist in the supplied AIR's
    /// `places[]` — validated at fire time by `parameterize_for_place`.
    pub air_target_place_id: String,
    /// Whether the trigger is live post-apply. Explicit (no default) so
    /// the deploy recipe must state intent — a disabled trigger never
    /// fires even if registered.
    pub enabled: bool,
}

/// Request body for `POST /api/templates/apply-air` — clinic-style
/// headless template upload.
///
/// Accepts pre-compiled AIR directly: no `WorkflowGraph` compile pass,
/// no Y.Doc init, no S3 file upload. The supplied `air_json` is stored
/// verbatim into the `air_json` column; a synthetic stub graph (one
/// Trigger node, no edges) is stored into the `graph` column so the
/// trigger dispatcher's `register_triggers` finds it.
///
/// Idempotency: name-based. Re-apply with the same `name` Bumps the
/// chain (new version row, prior version's triggers forgotten); first
/// apply Seeds (fresh chain at v1).
#[derive(Debug, Deserialize, ToSchema)]
pub struct ApplyAirTemplateRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Pre-compiled AIR. Stored verbatim. The endpoint runs no compile
    /// pass; the AIR is consumed by the engine at trigger-fire time.
    pub air_json: serde_json::Value,
    pub trigger: PreAirTriggerSpec,
    /// Optional git provenance, recorded into `source_ref` exactly like
    /// the existing GitOps `apply` endpoint.
    #[serde(default)]
    pub source_ref: Option<SourceRef>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTemplateRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub graph: Option<WorkflowGraph>,
    /// Optional per-node file map (filename → inline contents). Files are
    /// seeded as Y.Text entries inside each node's `files` Y.Map so that the
    /// new template lands ready-to-publish for backends that require
    /// attached scripts (e.g. Python's entrypoint).
    #[serde(default)]
    pub files: std::collections::HashMap<String, std::collections::HashMap<String, String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTemplateRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub graph: Option<WorkflowGraph>,
}

/// Template-specific list parameters layered on top of the generic
/// `crate::query::QueryParams` extractor (which owns `page`/`page_size`/`sort`/
/// `search`/`filter[field][op]`). These are the relational & security filters
/// that don't reduce to a plain column predicate: `project_id`/`tag` are M:N
/// joins, `base_template_id` switches the listing into version-chain mode, and
/// `owner_template_id` toggles private sub-workflow visibility.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct TemplateListExtras {
    /// Version-chain mode: list every version of this base family (ignoring
    /// `is_latest`) instead of the default latest-only catalogue listing.
    pub base_template_id: Option<Uuid>,
    /// Restrict to templates attached to a project (M:N via
    /// `project_templates.base_template_id`). The join is non-restrictive
    /// w.r.t. version chain — the live `is_latest` row wins.
    pub project_id: Option<Uuid>,
    /// Restrict to templates carrying this tag in the user's workspace.
    pub tag: Option<String>,
    /// Enumerate the private sub-workflow children owned by this parent
    /// family (`COALESCE(base_template_id, id)`). When supplied, the listing
    /// returns *only* those private children (they're otherwise hidden from
    /// the catalogue). When absent, private templates are excluded entirely.
    pub owner_template_id: Option<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PaginatedResponse<T: ToSchema> {
    pub items: Vec<T>,
    pub total: i64,
    pub page: i64,
    pub per_page: i64,
}

impl WorkflowGraph {
    /// Create a default graph with just a Start and End node connected by an edge.
    pub fn default_graph() -> Self {
        Self {
            nodes: vec![
                WorkflowNode {
                    id: "start".to_string(),
                    node_type: "start".to_string(),
                    slug: None,
                    position: Position { x: 250.0, y: 100.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial: Port::empty_input(),
                        process_name: None,
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "end".to_string(),
                    node_type: "end".to_string(),
                    slug: None,
                    position: Position { x: 250.0, y: 300.0 },
                    data: WorkflowNodeData::End {
                        label: "End".to_string(),
                        description: None,
                        terminal: default_terminal_port(),
                        result_mapping: Vec::new(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
            ],
            edges: vec![WorkflowEdge {
                id: "edge_start_to_end".to_string(),
                source: "start".to_string(),
                target: "end".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            }],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        }
    }
}

/// Single source of truth for the DSL (YAML/HCL) ↔ graph node mapping.
///
/// The CLI's `formats::dsl` module owns flow-string parsing, auto-layout and
/// the `DslWorkflow` envelope; the per-node payload mapping lives here, next
/// to [`WorkflowNodeData`], so a new enum variant fails to compile until
/// [`WorkflowNodeData::to_dsl_step`] handles it (no catch-all) and the
/// DSL→model direction can't silently swallow a known type.
pub mod dsl {
    use super::{
        default_join_output_port, default_max_turns, default_output_port, default_terminal_port,
        BranchCondition, ContextStrategy, DeploymentModel, ExecutionBackendType,
        ExecutionSpecConfig, JoinMode, LoopAccumulator, MergeStrategy, ModelRef,
        Port, RetryPolicy, TaskBlockConfig, TaskStepConfig, ToolErrorPolicy, WorkflowNode,
        WorkflowNodeData,
    };
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    pub struct DslStep {
        #[serde(rename = "type")]
        pub step_type: String,

        #[serde(skip_serializing_if = "Option::is_none")]
        pub label: Option<String>,

        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,

        // start
        #[serde(skip_serializing_if = "Option::is_none")]
        pub initial_data: Option<serde_json::Value>,

        /// Declared Start input-port shape. `None` means the step omitted it
        /// (legacy DSL files), in which case `from_dsl_step` falls back to the
        /// empty-input default — preserving prior behaviour. Round-trips the
        /// typed `initial` port that GUI-authored Starts carry.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub initial: Option<Port>,

        /// Optional Start process-name template (see
        /// `WorkflowNodeData::Start::process_name`). `None`/absent means no
        /// named-process registration, matching the historical DSL default.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub process_name: Option<String>,

        // human_task
        #[serde(skip_serializing_if = "Option::is_none")]
        pub task_title: Option<String>,

        #[serde(skip_serializing_if = "Option::is_none")]
        pub instructions: Option<String>,

        #[serde(skip_serializing_if = "Option::is_none")]
        pub steps: Option<Vec<DslTaskStep>>,

        /// Opt-in dynamic form: runtime `<slug>.<field>` ref for the step list.
        #[serde(rename = "stepsRef", default, skip_serializing_if = "Option::is_none")]
        pub steps_ref: Option<String>,

        // automated_step
        #[serde(skip_serializing_if = "Option::is_none")]
        pub execution: Option<DslExecution>,

        // agent
        #[serde(skip_serializing_if = "Option::is_none")]
        pub agent: Option<DslAgent>,

        // decision
        #[serde(skip_serializing_if = "Option::is_none")]
        pub conditions: Option<Vec<DslBranchCondition>>,

        #[serde(skip_serializing_if = "Option::is_none")]
        pub default_branch: Option<String>,

        // loop
        #[serde(skip_serializing_if = "Option::is_none")]
        pub max_iterations: Option<i32>,

        #[serde(skip_serializing_if = "Option::is_none")]
        pub loop_condition: Option<String>,

        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub accumulators: Vec<LoopAccumulator>,

        // scope
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub children: Vec<String>,

        #[serde(skip_serializing_if = "Option::is_none")]
        pub width: Option<f64>,

        #[serde(skip_serializing_if = "Option::is_none")]
        pub height: Option<f64>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DslTaskStep {
        pub title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub blocks: Option<Vec<serde_json::Value>>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DslExecution {
        pub backend: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub entrypoint: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub files: Vec<String>,
        pub config: serde_json::Value,
        /// Retry behaviour for the automated step. `None`/absent means the
        /// historical default (`RetryPolicy::default`, 3 immediate retries),
        /// so legacy DSL files keep their prior semantics.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub retry_policy: Option<RetryPolicy>,
    }

    /// DSL payload for an Agent step. Mirrors [`WorkflowNodeData::Agent`]
    /// 1:1 — same fields, same defaults — so a graph→DSL→graph round-trip
    /// is the identity. PR 1 only models the degenerate (single-turn) path
    /// at the compiler; the DSL surface stays full-fidelity so authoring
    /// future multi-turn agents needs no DSL schema change.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DslAgent {
        pub model: ModelRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub system_prompt: Option<String>,
        pub user_prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub response_format: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub images: Vec<serde_json::Value>,
        #[serde(default = "default_max_turns")]
        pub max_turns: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub stop_when: Option<String>,
        #[serde(default)]
        pub context_strategy: ContextStrategy,
        #[serde(default)]
        pub on_tool_error: ToolErrorPolicy,
        #[serde(default)]
        pub retry_policy: RetryPolicy,
        #[serde(default)]
        pub deployment_model: DeploymentModel,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct DslBranchCondition {
        pub edge: String,
        pub label: String,
        pub guard: String,
    }

    /// Synthesize a stable edge id from a source/target/handle triple.
    /// Mirrors the flow-parser's id scheme so DSL-declared decision branches
    /// resolve to the same edges the flow strings create.
    pub fn edge_id(source: &str, target: &str, handle: Option<&str>) -> String {
        match handle {
            Some(h) => format!("edge_{}_{}_to_{}", source, h, target),
            None => format!("edge_{}_to_{}", source, target),
        }
    }

    /// `snake_case` step key → `Title Case` label fallback.
    pub fn title_case(s: &str) -> String {
        s.split('_')
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    None => String::new(),
                    Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Extract the target step key from an auto-generated edge ID.
    /// e.g. `edge_check_yes_to_process` → `process`.
    pub fn extract_edge_target(edge_id: &str) -> String {
        if let Some(pos) = edge_id.rfind("_to_") {
            edge_id[pos + 4..].to_string()
        } else {
            edge_id.to_string()
        }
    }

    impl WorkflowNodeData {
        /// Build a node payload from a parsed DSL step. The `step_type`
        /// discriminator is data (it comes from YAML/HCL), so this arm is a
        /// string match — but every real variant is handled explicitly and
        /// the fallthrough is an error, never a silently-mistyped node.
        pub fn from_dsl_step(
            key: &str,
            step: &DslStep,
            label: &str,
        ) -> Result<WorkflowNodeData, String> {
            match step.step_type.as_str() {
                "start" => Ok(WorkflowNodeData::Start {
                    label: label.to_string(),
                    description: step.description.clone(),
                    // `initial_data` is the legacy read-compat blob (ignored
                    // here). Typed Start ports + process-name now round-trip
                    // via the dedicated `initial` / `process_name` fields;
                    // absent (legacy files) falls back to the empty-input
                    // default so older templates load unchanged.
                    initial: step.initial.clone().unwrap_or_else(Port::empty_input),
                    process_name: step.process_name.clone(),
                }),
                "end" => Ok(WorkflowNodeData::End {
                    label: label.to_string(),
                    description: step.description.clone(),
                    terminal: default_terminal_port(),
                    result_mapping: Vec::new(),
                }),
                "human_task" => {
                    let task_steps = step
                        .steps
                        .as_ref()
                        .map(|dsl_steps| {
                            dsl_steps
                                .iter()
                                .enumerate()
                                .map(|(i, ds)| {
                                    let blocks: Vec<TaskBlockConfig> = ds
                                        .blocks
                                        .as_ref()
                                        .map(|b| {
                                            b.iter()
                                                .filter_map(|v| {
                                                    serde_json::from_value(v.clone()).ok()
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    TaskStepConfig {
                                        id: format!("{}-step-{}", key, i),
                                        title: ds.title.clone(),
                                        description_mdsvex: ds.description.clone(),
                                        blocks,
                                    }
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    Ok(WorkflowNodeData::HumanTask {
                        label: label.to_string(),
                        description: step.description.clone(),
                        task_title: step.task_title.clone().unwrap_or_else(|| label.to_string()),
                        instructions_mdsvex: step.instructions.clone(),
                        steps: task_steps,
                        steps_ref: step.steps_ref.clone(),
                    })
                }
                "agent" => {
                    let a = step
                        .agent
                        .as_ref()
                        .ok_or_else(|| format!("agent '{}' requires an 'agent' field", key))?;
                    Ok(WorkflowNodeData::Agent {
                        label: label.to_string(),
                        description: step.description.clone(),
                        model: a.model.clone(),
                        system_prompt: a.system_prompt.clone(),
                        user_prompt: a.user_prompt.clone(),
                        response_format: a.response_format.clone(),
                        images: a.images.clone(),
                        max_turns: a.max_turns,
                        stop_when: a.stop_when.clone(),
                        context_strategy: a.context_strategy,
                        on_tool_error: a.on_tool_error,
                        retry_policy: a.retry_policy,
                        deployment_model: a.deployment_model.clone(),
                        // DSL does not model asset bindings (yet).
                        asset_bindings: Vec::new(),
                    })
                }
                "automated_step" => {
                    let exec = step.execution.as_ref().ok_or_else(|| {
                        format!("automated_step '{}' requires an 'execution' field", key)
                    })?;
                    // Merge entrypoint and files list into config
                    let mut config = exec.config.clone();
                    if let serde_json::Value::Object(ref mut map) = config {
                        if let Some(ref ep) = exec.entrypoint {
                            map.insert(
                                "entrypoint".to_string(),
                                serde_json::Value::String(ep.clone()),
                            );
                        }
                        if !exec.files.is_empty() {
                            let files_arr: Vec<serde_json::Value> = exec
                                .files
                                .iter()
                                .map(|f| serde_json::Value::String(f.clone()))
                                .collect();
                            map.insert(
                                "required_files".to_string(),
                                serde_json::Value::Array(files_arr),
                            );
                        }
                    }
                    // Parse the backend discriminator via serde — keeps the
                    // DSL's accepted value set in lockstep with the wire enum.
                    let backend_type: ExecutionBackendType = serde_json::from_value(
                        serde_json::Value::String(exec.backend.clone()),
                    )
                    .map_err(|_| {
                        format!(
                            "automated_step '{}' has unknown backend '{}' (expected one of: python, process, docker, http, llm, file_ops, kreuzberg, smtp)",
                            key, exec.backend
                        )
                    })?;
                    Ok(WorkflowNodeData::AutomatedStep {
                        label: label.to_string(),
                        description: step.description.clone(),
                        execution_spec: ExecutionSpecConfig {
                            backend_type,
                            entrypoint: None,
                            config,
                        },
                        input: Port::empty_input(),
                        output: default_output_port(backend_type),
                        // Absent (legacy DSL) → historical default of 3
                        // immediate retries; otherwise round-trip the
                        // authored policy.
                        retry_policy: exec.retry_policy.unwrap_or_default(),
                        // DSL does not model deployment topology — inline.
                        deployment_model: DeploymentModel::default(),
                        // DSL does not model resource pools (yet).
                        // DSL does not model streaming output (prototype flag).
                        stream_output: false,
                        // DSL does not model streaming input (reducer flag).
                        stream_input: false,
                        requirements: None,
                        // DSL does not model asset bindings (yet).
                        asset_bindings: Vec::new(),
                    })
                }
                "decision" => {
                    let dsl_conditions = step.conditions.as_ref().cloned().unwrap_or_default();
                    let conditions: Vec<BranchCondition> = dsl_conditions
                        .iter()
                        .map(|dc| {
                            let eid = edge_id(
                                key,
                                &dc.edge,
                                Some(&dc.label.to_lowercase().replace(' ', "_")),
                            );
                            BranchCondition {
                                edge_id: eid,
                                label: dc.label.clone(),
                                guard: dc.guard.clone(),
                            }
                        })
                        .collect();

                    let default_branch = step
                        .default_branch
                        .as_ref()
                        .map(|target| edge_id(key, target, None));

                    Ok(WorkflowNodeData::Decision {
                        label: label.to_string(),
                        description: step.description.clone(),
                        conditions,
                        default_branch,
                    })
                }
                "parallel_split" => Ok(WorkflowNodeData::ParallelSplit {
                    label: label.to_string(),
                    description: step.description.clone(),
                }),
                "join" => Ok(WorkflowNodeData::Join {
                    label: label.to_string(),
                    description: step.description.clone(),
                    mode: JoinMode::default(),
                    merge_strategy: Some(MergeStrategy::default()),
                    output: default_join_output_port(),
                }),
                "loop" => {
                    let max_iter = step
                        .max_iterations
                        .ok_or_else(|| format!("loop '{}' requires 'max_iterations'", key))?;
                    let condition = step
                        .loop_condition
                        .clone()
                        .ok_or_else(|| format!("loop '{}' requires 'loop_condition'", key))?;
                    Ok(WorkflowNodeData::Loop {
                        label: label.to_string(),
                        description: step.description.clone(),
                        max_iterations: max_iter,
                        loop_condition: condition,
                        accumulators: step.accumulators.clone(),
                    })
                }
                "scope" => Ok(WorkflowNodeData::Scope {
                    label: label.to_string(),
                    description: step.description.clone(),
                }),
                // The process-control + trigger nodes are GUI-authored: the
                // DSL has no schema for their required fields, and
                // `to_dsl_step` drops them on the way out (documented lossy).
                // They previously fell into the generic catch-all error; keep
                // that behaviour but make it explicit per kind so the
                // round-trip asymmetry is greppable rather than silent.
                "phase_update" | "progress_update" | "failure" | "trigger" | "delay"
                | "timeout" | "map" => Err(format!(
                    "step '{}' has GUI-only type '{}' which the DSL format does not model",
                    key, step.step_type
                )),
                other => Err(format!("unknown step type '{}' for step '{}'", other, key)),
            }
        }

        /// Project this node payload onto a fresh [`DslStep`]. Exhaustive
        /// `match self` — adding a [`WorkflowNodeData`] variant is a compile
        /// error here until the new variant declares how it serializes (or
        /// explicitly that it's GUI-only and dropped).
        pub fn to_dsl_step(&self, node: &WorkflowNode) -> DslStep {
            let mut step = DslStep {
                step_type: node.node_type.clone(),
                label: Some(self.label().to_string()),
                description: self.description().map(|s| s.to_string()),
                initial_data: None,
                initial: None,
                process_name: None,
                task_title: None,
                instructions: None,
                steps: None,
                steps_ref: None,
                execution: None,
                agent: None,
                conditions: None,
                default_branch: None,
                max_iterations: None,
                loop_condition: None,
                accumulators: Vec::new(),
                children: Vec::new(),
                width: node.width,
                height: node.height,
            };

            match self {
                WorkflowNodeData::Start {
                    initial,
                    process_name,
                    ..
                } => {
                    step.initial = Some(initial.clone());
                    step.process_name = process_name.clone();
                }
                WorkflowNodeData::End { .. } => {}
                WorkflowNodeData::HumanTask {
                    task_title,
                    instructions_mdsvex,
                    steps: task_steps,
                    steps_ref,
                    ..
                } => {
                    step.task_title = Some(task_title.clone());
                    step.instructions = instructions_mdsvex.clone();
                    step.steps_ref = steps_ref.clone();
                    if !task_steps.is_empty() {
                        step.steps = Some(
                            task_steps
                                .iter()
                                .map(|ts| DslTaskStep {
                                    title: ts.title.clone(),
                                    description: ts.description_mdsvex.clone(),
                                    blocks: if ts.blocks.is_empty() {
                                        None
                                    } else {
                                        Some(
                                            ts.blocks
                                                .iter()
                                                .filter_map(|b| serde_json::to_value(b).ok())
                                                .collect(),
                                        )
                                    },
                                })
                                .collect(),
                        );
                    }
                }
                WorkflowNodeData::AutomatedStep {
                    execution_spec,
                    retry_policy,
                    ..
                } => {
                    // Extract entrypoint and files from config into their own
                    // fields
                    let mut config = execution_spec.config.clone();
                    let (entrypoint, files) = if let serde_json::Value::Object(ref mut map) = config
                    {
                        let ep = map
                            .remove("entrypoint")
                            .and_then(|v| v.as_str().map(|s| s.to_string()));
                        let f = map
                            .remove("required_files")
                            .and_then(|v| {
                                v.as_array().map(|arr| {
                                    arr.iter()
                                        .filter_map(|item| item.as_str().map(|s| s.to_string()))
                                        .collect()
                                })
                            })
                            .unwrap_or_default();
                        (ep, f)
                    } else {
                        (None, vec![])
                    };
                    // Round-trip the enum through serde to recover the
                    // canonical snake_case wire string (`python`, `file_ops`,
                    // …) so the DSL export matches what users would type.
                    let backend = serde_json::to_value(execution_spec.backend_type)
                        .ok()
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_default();
                    step.execution = Some(DslExecution {
                        backend,
                        entrypoint,
                        files,
                        config,
                        retry_policy: Some(*retry_policy),
                    });
                }
                WorkflowNodeData::Decision {
                    conditions,
                    default_branch,
                    ..
                } => {
                    if !conditions.is_empty() {
                        step.conditions = Some(
                            conditions
                                .iter()
                                .map(|bc| DslBranchCondition {
                                    edge: extract_edge_target(&bc.edge_id),
                                    label: bc.label.clone(),
                                    guard: bc.guard.clone(),
                                })
                                .collect(),
                        );
                    }
                    if let Some(db) = default_branch {
                        step.default_branch = Some(extract_edge_target(db));
                    }
                }
                WorkflowNodeData::ParallelSplit { .. } => {}
                WorkflowNodeData::Join { .. } => {
                    // Join's mode/merge_strategy/output are GUI-only for now —
                    // the DSL has no schema for them. Round-trip through DSL
                    // drops the join-specific config, mirroring how
                    // process-control nodes behave.
                }
                WorkflowNodeData::Scope { .. } => {
                    // children are populated by the CLI envelope after the
                    // step map is built
                }
                WorkflowNodeData::LeaseScope { .. } => {
                    // children are populated by the CLI envelope after the
                    // step map is built; LeaseScope is GUI-authored for now
                    // (DSL doesn't model container nodes with lease bindings).
                }
                WorkflowNodeData::Loop {
                    max_iterations,
                    loop_condition,
                    accumulators,
                    ..
                } => {
                    step.max_iterations = Some(*max_iterations);
                    step.loop_condition = Some(loop_condition.clone());
                    step.accumulators = accumulators.clone();
                }
                WorkflowNodeData::PhaseUpdate { .. }
                | WorkflowNodeData::ProgressUpdate { .. }
                | WorkflowNodeData::Failure { .. }
                | WorkflowNodeData::Delay { .. }
                | WorkflowNodeData::Timeout { .. }
                | WorkflowNodeData::Map { .. }
                | WorkflowNodeData::StreamFold { .. } => {
                    // DSL doesn't model the process-control / container nodes —
                    // GUI-authored for now. Same lossy-drop behaviour as
                    // triggers. (Map's body sub-graph + itemsRef/resultVar, and
                    // StreamConsumer/StreamFold's resultVar/reduce, have no DSL
                    // schema yet.)
                }
                WorkflowNodeData::Agent {
                    model,
                    system_prompt,
                    user_prompt,
                    response_format,
                    images,
                    max_turns,
                    stop_when,
                    context_strategy,
                    on_tool_error,
                    retry_policy,
                    deployment_model,
                    ..
                } => {
                    step.agent = Some(DslAgent {
                        model: model.clone(),
                        system_prompt: system_prompt.clone(),
                        user_prompt: user_prompt.clone(),
                        response_format: response_format.clone(),
                        images: images.clone(),
                        max_turns: *max_turns,
                        stop_when: stop_when.clone(),
                        context_strategy: *context_strategy,
                        on_tool_error: *on_tool_error,
                        retry_policy: *retry_policy,
                        deployment_model: deployment_model.clone(),
                    });
                }
                WorkflowNodeData::Trigger { .. } | WorkflowNodeData::SubWorkflow { .. } => {
                    // DSL doesn't model triggers or sub-workflows — declared in
                    // the GUI for now. Round-trip through DSL drops them,
                    // matching how legacy DSL templates behave.
                }
            }

            step
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pf(name: &str, kind: FieldKind, required: bool) -> PortField {
        PortField {
            schema: None,
            name: name.to_string(),
            label: name.to_string(),
            kind,
            required,
            options: None,
            description: None,
            accept: None,
        }
    }

    // ── TaskFieldKind / TaskFieldConfig type-parity tests ─────────────
    //
    // These pin the wire-shape sync between the compiler-side
    // `TaskFieldKind` (this file) and the engine-side equivalent in
    // `engine/core-engine/crates/domain/src/human.rs`. The two have to
    // agree exactly — a kind the compiler accepts but the engine
    // rejects wedges a live net at the human-task effect handler.
    //
    // The frontend's `TASK_FIELD_KINDS` in
    // `app/src/lib/hpi/types.ts` is the third leg; it isn't
    // auto-generated but is asserted against the OpenAPI schema in CI's
    // `openapi-drift` check.

    #[test]
    fn task_field_kind_all_variants_round_trip_through_json() {
        // Every TaskFieldKind round-trips through serde with its
        // snake_case wire form. Adding a new variant without serde
        // tagging or with a wrong rename_all here would slip past type
        // checks but fail at compile-to-air time.
        let cases = [
            (TaskFieldKind::Text, "\"text\""),
            (TaskFieldKind::Textarea, "\"textarea\""),
            (TaskFieldKind::Number, "\"number\""),
            (TaskFieldKind::Select, "\"select\""),
            (TaskFieldKind::Checkbox, "\"checkbox\""),
            (TaskFieldKind::File, "\"file\""),
            (TaskFieldKind::Signature, "\"signature\""),
            (TaskFieldKind::Radio, "\"radio\""),
            (TaskFieldKind::Date, "\"date\""),
            (TaskFieldKind::Range, "\"range\""),
            (TaskFieldKind::Rating, "\"rating\""),
        ];
        for (kind, wire) in cases {
            let ser = serde_json::to_string(&kind).expect("serialize");
            assert_eq!(ser, wire, "wire form drift for {kind:?}");
            let back: TaskFieldKind = serde_json::from_str(wire).expect("deserialize");
            assert_eq!(back, kind, "round-trip drift for {wire}");
        }
    }

    #[test]
    fn task_field_kind_maps_to_typed_port_field_kind() {
        // The compiler emits a typed `Port` for the HumanTask's parked
        // output by mapping each Input block's field kind through this
        // From impl. Pin the mapping so downstream borrow-checking can
        // rely on the typed-ports superset (`Bool` for checkbox, etc.).
        assert_eq!(FieldKind::from(TaskFieldKind::Text), FieldKind::Text);
        assert_eq!(
            FieldKind::from(TaskFieldKind::Textarea),
            FieldKind::Textarea
        );
        assert_eq!(FieldKind::from(TaskFieldKind::Number), FieldKind::Number);
        assert_eq!(FieldKind::from(TaskFieldKind::Select), FieldKind::Select);
        assert_eq!(FieldKind::from(TaskFieldKind::Checkbox), FieldKind::Bool);
        assert_eq!(FieldKind::from(TaskFieldKind::File), FieldKind::File);
        assert_eq!(
            FieldKind::from(TaskFieldKind::Signature),
            FieldKind::Signature
        );
        // New in Feature B parity sync: Radio borrows Select's option
        // semantics, Date is wire-text (ISO string), Range/Rating emit
        // plain numbers.
        assert_eq!(FieldKind::from(TaskFieldKind::Radio), FieldKind::Select);
        assert_eq!(FieldKind::from(TaskFieldKind::Date), FieldKind::Text);
        assert_eq!(FieldKind::from(TaskFieldKind::Range), FieldKind::Number);
        assert_eq!(FieldKind::from(TaskFieldKind::Rating), FieldKind::Number);
    }

    #[test]
    fn task_field_config_renderer_metadata_round_trips() {
        // Authors set min/max/step on a Range field (or max_rating on a
        // Rating field, or include_time on a Date field). The compiler
        // must serialize these so the engine can forward them to the
        // renderer — otherwise the per-field customization disappears
        // between editor and run.
        let raw = serde_json::json!({
            "name": "score",
            "label": "Score",
            "kind": "range",
            "required": true,
            "min": 0,
            "max": 10,
            "step": 0.5,
        });
        let field: TaskFieldConfig =
            serde_json::from_value(raw.clone()).expect("range field parses");
        assert_eq!(field.kind, TaskFieldKind::Range);
        assert_eq!(field.min, Some(0.0));
        assert_eq!(field.max, Some(10.0));
        assert_eq!(field.step, Some(0.5));
        // And round-trip: re-serializing must preserve the metadata.
        let back = serde_json::to_value(&field).expect("serialize");
        assert_eq!(back["min"], 0.0);
        assert_eq!(back["max"], 10.0);
        assert_eq!(back["step"], 0.5);
    }

    #[test]
    fn task_field_config_omits_unset_metadata_from_wire() {
        // skip_serializing_if = "Option::is_none" must hold for every
        // optional metadata key — otherwise byte-identity guards for
        // legacy TaskField shapes break and OpenAPI drift cycles
        // forever as tests churn the spec.
        let field = TaskFieldConfig {
            name: "x".into(),
            label: "X".into(),
            kind: TaskFieldKind::Text,
            required: None,
            placeholder: None,
            description_mdsvex: None,
            options: None,
            accept: None,
            max_file_size: None,
            max_files: None,
            signature_mode: None,
            pen_color: None,
            min: None,
            max: None,
            step: None,
            max_rating: None,
            include_time: None,
        };
        let wire = serde_json::to_value(&field).expect("serialize");
        let obj = wire.as_object().expect("object");
        // Only name + label + kind survive when nothing else is set.
        let mut keys: Vec<&str> = obj.keys().map(|s| s.as_str()).collect();
        keys.sort();
        assert_eq!(keys, vec!["kind", "label", "name"]);
    }

    #[test]
    fn workflow_graph_definitions_roundtrip() {
        let mut defs = std::collections::BTreeMap::new();
        defs.insert(
            "ExtractionFields".to_string(),
            serde_json::json!({
                "type": "object",
                "properties": {
                    "fields": { "type": "array", "items": { "type": "object" } }
                }
            }),
        );
        let graph = WorkflowGraph {
            nodes: vec![],
            edges: vec![],
            viewport: None,
            instance_concurrency: InstanceConcurrencyPolicy::Unlimited,
            definitions: defs,
            default_scheduler: None,
        };
        let s = serde_json::to_string(&graph).unwrap();
        let parsed: WorkflowGraph = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.definitions.len(), 1);
        assert!(parsed.definitions.contains_key("ExtractionFields"));
        assert_eq!(
            parsed.definitions["ExtractionFields"]["properties"]["fields"]["type"],
            "array"
        );

        let empty = WorkflowGraph {
            nodes: vec![],
            edges: vec![],
            viewport: None,
            instance_concurrency: InstanceConcurrencyPolicy::Unlimited,
            definitions: std::collections::BTreeMap::new(),
            default_scheduler: None,
        };
        let s2 = serde_json::to_string(&empty).unwrap();
        assert!(!s2.contains("definitions"));
    }

    #[test]
    fn validate_token_accepts_well_typed_object() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![
                pf("name", FieldKind::Text, true),
                pf("count", FieldKind::Number, false),
                pf("blob", FieldKind::Json, false),
            ],
        };
        let ok = serde_json::json!({ "name": "a", "count": 3, "blob": [1, 2] });
        assert!(port.validate_token(&ok).is_ok());
        let ok2 = serde_json::json!({ "name": "a" });
        assert!(port.validate_token(&ok2).is_ok());
    }

    #[test]
    fn validate_token_rejects_missing_required_and_kind_mismatch() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![
                pf("name", FieldKind::Text, true),
                pf("n", FieldKind::Number, false),
            ],
        };
        match port.validate_token(&serde_json::json!({ "n": 1 })) {
            Err(PortValidationError::MissingRequiredField { field }) => assert_eq!(field, "name"),
            other => panic!("expected MissingRequiredField, got {other:?}"),
        }
        match port.validate_token(&serde_json::json!({ "name": "a", "n": "5" })) {
            Err(PortValidationError::FieldKindMismatch { field, kind }) => {
                assert_eq!(field, "n");
                assert!(matches!(kind, FieldKind::Number));
            }
            other => panic!("expected FieldKindMismatch, got {other:?}"),
        }
        assert!(matches!(
            port.validate_token(&serde_json::json!([1, 2])),
            Err(PortValidationError::NotObject)
        ));
    }

    #[test]
    fn validate_token_fieldless_port_accepts_any_object() {
        let port = Port::empty_input();
        assert!(port
            .validate_token(&serde_json::json!({ "anything": 1 }))
            .is_ok());
        assert!(port.validate_token(&serde_json::json!({})).is_ok());
        assert!(matches!(
            port.validate_token(&serde_json::json!("nope")),
            Err(PortValidationError::NotObject)
        ));
    }

    #[test]
    fn scope_node_data_roundtrip() {
        let data = WorkflowNodeData::Scope {
            label: "My Scope".to_string(),
            description: Some("A visual container".to_string()),
        };
        let json = serde_json::to_value(&data).unwrap();
        assert_eq!(json["type"], "scope");
        assert_eq!(json["label"], "My Scope");
        assert_eq!(json["description"], "A visual container");

        let back: WorkflowNodeData = serde_json::from_value(json).unwrap();
        assert_eq!(back.type_name(), "scope");
        assert_eq!(back.label(), "My Scope");
        assert_eq!(back.description(), Some("A visual container"));
    }

    #[test]
    fn workflow_node_with_parent_id_roundtrip() {
        let node = WorkflowNode {
            id: "child".to_string(),
            node_type: "human_task".to_string(),
            slug: None,
            position: Position { x: 10.0, y: 20.0 },
            data: WorkflowNodeData::HumanTask {
                label: "Task".to_string(),
                description: None,
                task_title: "Do it".to_string(),
                instructions_mdsvex: None,
                steps: vec![],
                steps_ref: None,
            },
            parent_id: Some("scope1".to_string()),
            width: None,
            height: None,
        };
        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json["parentId"], "scope1");

        let back: WorkflowNode = serde_json::from_value(json).unwrap();
        assert_eq!(back.parent_id, Some("scope1".to_string()));
    }

    #[test]
    fn scope_node_with_dimensions_roundtrip() {
        let node = WorkflowNode {
            id: "s1".to_string(),
            node_type: "scope".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::Scope {
                label: "Container".to_string(),
                description: None,
            },
            parent_id: None,
            width: Some(500.0),
            height: Some(300.0),
        };
        let json = serde_json::to_value(&node).unwrap();
        assert_eq!(json["width"], 500.0);
        assert_eq!(json["height"], 300.0);
        assert!(json.get("parentId").is_none());

        let back: WorkflowNode = serde_json::from_value(json).unwrap();
        assert_eq!(back.width, Some(500.0));
        assert_eq!(back.height, Some(300.0));
        assert_eq!(back.parent_id, None);
    }

    #[test]
    fn parent_id_omitted_when_none() {
        let node = WorkflowNode {
            id: "n".to_string(),
            node_type: "end".to_string(),
            slug: None,
            position: Position { x: 0.0, y: 0.0 },
            data: WorkflowNodeData::End {
                label: "End".to_string(),
                description: None,
                terminal: default_terminal_port(),
                result_mapping: Vec::new(),
            },
            parent_id: None,
            width: None,
            height: None,
        };
        let json = serde_json::to_string(&node).unwrap();
        assert!(
            !json.contains("parentId"),
            "parentId should be omitted when None"
        );
        assert!(!json.contains("width"), "width should be omitted when None");
        assert!(
            !json.contains("height"),
            "height should be omitted when None"
        );
    }

    #[test]
    fn image_block_roundtrip() {
        let block = TaskBlockConfig::Image {
            filenames: vec!["photo.png".to_string(), "diagram.svg".to_string()],
            display: ImageDisplay::Grid,
            url: None,
            alt: None,
            caption: None,
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "image");
        assert_eq!(json["filenames"][0], "photo.png");
        assert_eq!(json["filenames"][1], "diagram.svg");
        assert_eq!(json["display"], "grid");
        // Additive url/alt/caption stay absent from the wire when unset.
        assert!(json.get("url").is_none());

        let back: TaskBlockConfig = serde_json::from_value(json).unwrap();
        if let TaskBlockConfig::Image {
            filenames,
            display,
            url,
            ..
        } = back
        {
            assert_eq!(filenames.len(), 2);
            assert_eq!(display, ImageDisplay::Grid);
            assert_eq!(url, None);
        } else {
            panic!("expected Image variant");
        }
    }

    #[test]
    fn url_image_and_download_blocks_match_frontend_shape() {
        // url-driven image: filenames empty (omitted), url present.
        let img = TaskBlockConfig::Image {
            filenames: vec![],
            display: ImageDisplay::Single,
            url: Some("/api/v1/files/k.png".to_string()),
            alt: Some("Invoice".to_string()),
            caption: None,
        };
        let j = serde_json::to_value(&img).unwrap();
        assert_eq!(j["type"], "image");
        assert_eq!(j["url"], "/api/v1/files/k.png");
        assert!(j.get("filenames").is_none(), "empty filenames omitted: {j}");
        assert!(j.get("caption").is_none());

        let dl = TaskBlockConfig::Download {
            downloads: vec![DownloadItemConfig {
                url: "/api/v1/files/k.pdf".to_string(),
                filename: "invoice.pdf".to_string(),
                size: None,
                mime_type: Some("application/pdf".to_string()),
                thumbnail_url: None,
                description: Some("Original invoice".to_string()),
            }],
        };
        let j = serde_json::to_value(&dl).unwrap();
        assert_eq!(j["type"], "download");
        assert_eq!(j["downloads"][0]["url"], "/api/v1/files/k.pdf");
        assert_eq!(j["downloads"][0]["mime_type"], "application/pdf");
        assert!(j["downloads"][0].get("size").is_none());

        // Round-trips back to the same variants.
        let back: TaskBlockConfig = serde_json::from_value(j).unwrap();
        assert!(matches!(back, TaskBlockConfig::Download { .. }));
    }

    /// Canary: each newly-enumified field must serialize to the same wire
    /// strings it produced when typed as `String`. If this fails, the JSON in
    /// the database / on the network has diverged from the Rust type.
    #[test]
    fn enumified_fields_preserve_wire_format() {
        // Callout severity
        let json = serde_json::json!({
            "type": "callout",
            "severity": "warning",
            "content": "hi",
        });
        let block: TaskBlockConfig = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&block).unwrap(), json);

        // Image display + nested field kind for full coverage
        let json = serde_json::json!({
            "type": "image",
            "filenames": ["a.png"],
            "display": "gallery",
        });
        let block: TaskBlockConfig = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&block).unwrap(), json);

        // TaskFieldKind via the input variant
        let json = serde_json::json!({
            "type": "input",
            "field": {
                "name": "n",
                "label": "L",
                "kind": "textarea",
            },
        });
        let block: TaskBlockConfig = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&block).unwrap(), json);

        // ExecutionBackendType snake_case rename (file_ops is the canary —
        // PascalCase would emit `"fileOps"` and break the wire).
        let spec = ExecutionSpecConfig {
            backend_type: ExecutionBackendType::FileOps,
            entrypoint: None,
            config: serde_json::json!({}),
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["backendType"], "file_ops");
        let back: ExecutionSpecConfig = serde_json::from_value(json).unwrap();
        assert_eq!(back.backend_type, ExecutionBackendType::FileOps);
    }

    #[test]
    fn file_block_roundtrip() {
        let block = TaskBlockConfig::File {
            filename: "report.pdf".to_string(),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "file");
        assert_eq!(json["filename"], "report.pdf");

        let back: TaskBlockConfig = serde_json::from_value(json).unwrap();
        if let TaskBlockConfig::File { filename } = back {
            assert_eq!(filename, "report.pdf");
        } else {
            panic!("expected File variant");
        }
    }

    #[test]
    fn all_block_types_deserialize() {
        // Verify all block types roundtrip through JSON
        let blocks = [
            serde_json::json!({"type": "input", "field": {"name": "f", "label": "F", "kind": "text"}}),
            serde_json::json!({"type": "mdsvex", "content": "# Hello"}),
            serde_json::json!({"type": "callout", "severity": "warning", "content": "Watch out"}),
            serde_json::json!({"type": "divider"}),
            serde_json::json!({"type": "image", "filenames": ["a.png"], "display": "single"}),
            serde_json::json!({"type": "file", "filename": "data.csv"}),
        ];
        for (i, json) in blocks.iter().enumerate() {
            let result: Result<TaskBlockConfig, _> = serde_json::from_value(json.clone());
            assert!(
                result.is_ok(),
                "block type {} failed to deserialize: {:?}",
                i,
                result.err()
            );
        }
    }

    #[test]
    fn source_ref_jsonb_roundtrip() {
        // What `apply` serializes into the `source_ref` JSONB column. `ref`
        // is renamed and omitted when None; `dirty` is always present.
        let sr = SourceRef {
            remote: "git@forge.aithericon.eu:Milan/wf.git".to_string(),
            sha: "a1b2c3d4".to_string(),
            dirty: true,
            git_ref: Some("main".to_string()),
        };
        let v = serde_json::to_value(&sr).unwrap();
        assert_eq!(v["remote"], "git@forge.aithericon.eu:Milan/wf.git");
        assert_eq!(v["sha"], "a1b2c3d4");
        assert_eq!(v["dirty"], true);
        assert_eq!(v["ref"], "main");
        let back: SourceRef = serde_json::from_value(v).unwrap();
        assert_eq!(back.sha, "a1b2c3d4");
        assert_eq!(back.git_ref.as_deref(), Some("main"));

        let none = SourceRef {
            remote: "r".to_string(),
            sha: "s".to_string(),
            dirty: false,
            git_ref: None,
        };
        let v = serde_json::to_value(&none).unwrap();
        assert!(v.get("ref").is_none(), "ref must be omitted when None");
        let back: SourceRef = serde_json::from_value(v).unwrap();
        assert_eq!(back.git_ref, None);
    }
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use serde_json::json;

    fn pf(name: &str, kind: FieldKind, required: bool) -> PortField {
        PortField {
            schema: None,
            name: name.to_string(),
            label: name.to_string(),
            kind,
            required,
            options: None,
            description: None,
            accept: None,
        }
    }

    fn base_type(kind: FieldKind) -> Option<String> {
        kind.base_schema()
            .get("type")
            .and_then(|t| t.as_str())
            .map(str::to_string)
    }

    /// Anti-drift: `accepts` and `base_schema` are derived from the same
    /// FieldKind switch and must agree on a representative value per kind.
    #[test]
    fn accepts_agrees_with_base_schema() {
        assert!(FieldKind::Number.accepts(&json!(3)));
        assert_eq!(base_type(FieldKind::Number).as_deref(), Some("number"));

        assert!(FieldKind::Bool.accepts(&json!(true)));
        assert_eq!(base_type(FieldKind::Bool).as_deref(), Some("boolean"));

        assert!(FieldKind::Text.accepts(&json!("x")));
        assert_eq!(base_type(FieldKind::Text).as_deref(), Some("string"));

        assert!(FieldKind::Timestamp.accepts(&json!("2026-01-01T00:00:00Z")));
        assert_eq!(base_type(FieldKind::Timestamp).as_deref(), Some("string"));

        // Json accepts anything and emits the opaque `{}`.
        assert!(FieldKind::Json.accepts(&json!({"any": [1, 2, 3]})));
        assert!(FieldKind::Json.accepts(&json!("scalar")));
        assert_eq!(FieldKind::Json.base_schema(), json!({}));

        // Object accepts only JSON objects; emits a permissive object base.
        assert!(FieldKind::Object.accepts(&json!({"k": 1})));
        assert!(!FieldKind::Object.accepts(&json!([1, 2])));
        assert!(!FieldKind::Object.accepts(&json!("x")));
        assert_eq!(base_type(FieldKind::Object).as_deref(), Some("object"));
        assert_eq!(
            FieldKind::Object.base_schema()["additionalProperties"],
            json!(true)
        );

        // Array accepts only JSON arrays; emits a permissive array base.
        assert!(FieldKind::Array.accepts(&json!([1, 2, 3])));
        assert!(!FieldKind::Array.accepts(&json!({"k": 1})));
        assert!(!FieldKind::Array.accepts(&json!("x")));
        assert_eq!(base_type(FieldKind::Array).as_deref(), Some("array"));
    }

    #[test]
    fn object_array_fields_emit_permissive_schema_without_override() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![
                pf("payload", FieldKind::Object, false),
                pf("items", FieldKind::Array, false),
            ],
        };
        let schema = port.json_schema();
        assert_eq!(
            schema["properties"]["payload"],
            json!({"type": "object", "additionalProperties": true})
        );
        assert_eq!(schema["properties"]["items"], json!({"type": "array"}));
    }

    #[test]
    fn object_array_fields_emit_schema_override_verbatim() {
        let nested = json!({
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "qty": {"type": "number"},
            },
            "required": ["id"],
            "additionalProperties": false,
        });
        let arr = json!({
            "type": "array",
            "items": {"type": "string"},
            "minItems": 1,
        });
        let mut obj_field = pf("payload", FieldKind::Object, false);
        obj_field.schema = Some(nested.clone());
        let mut arr_field = pf("items", FieldKind::Array, false);
        arr_field.schema = Some(arr.clone());
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![obj_field, arr_field],
        };
        let schema = port.json_schema();
        // The author override wins verbatim — constraints (`required`,
        // `minItems`) are preserved for the runtime SchemaRegistry.
        assert_eq!(schema["properties"]["payload"], nested);
        assert_eq!(schema["properties"]["items"], arr);
    }

    #[test]
    fn port_json_schema_required_only_for_required_fields() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![
                pf("name", FieldKind::Text, true),
                pf("note", FieldKind::Text, false),
            ],
        };
        let schema = port.json_schema();
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["name"]));
        assert_eq!(schema["properties"]["name"]["type"], json!("string"));
    }

    #[test]
    fn port_json_schema_omits_required_when_none() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![pf("note", FieldKind::Text, false)],
        };
        let schema = port.json_schema();
        assert!(
            schema.get("required").is_none(),
            "required must be omitted when no field is required"
        );
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn port_json_schema_empty_port_is_permissive() {
        let port = Port {
            id: "in".into(),
            label: "In".into(),
            fields: vec![],
        };
        let schema = port.json_schema();
        assert_eq!(
            schema,
            json!({"type": "object", "additionalProperties": true})
        );
    }

    #[test]
    fn select_field_with_options_emits_enum() {
        let mut field = pf("choice", FieldKind::Select, false);
        field.options = Some(vec![
            SelectOption {
                value: "approve".into(),
                label: "Approve".into(),
            },
            SelectOption {
                value: "reject".into(),
                label: "Reject".into(),
            },
        ]);
        let schema = field.kind.json_schema(&field);
        assert_eq!(schema["type"], json!("string"));
        assert_eq!(schema["enum"], json!(["approve", "reject"]));
    }

    #[test]
    fn field_schema_override_wins_verbatim() {
        let mut field = pf("steps", FieldKind::Json, false);
        let custom = json!({"type": "array", "items": {"type": "object"}});
        field.schema = Some(custom.clone());
        assert_eq!(field.kind.json_schema(&field), custom);
    }

    #[test]
    fn description_is_attached() {
        let mut field = pf("name", FieldKind::Text, false);
        field.description = Some("the customer name".into());
        let schema = field.kind.json_schema(&field);
        assert_eq!(schema["description"], json!("the customer name"));
    }
}
