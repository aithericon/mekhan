//! Template DB row + the visual-editor graph model: [`WorkflowTemplate`],
//! [`WorkflowGraph`], nodes/edges and the whole [`WorkflowNodeData`] enum.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::agent::{default_max_turns, ContextStrategy, ModelRef, ToolErrorPolicy};
use super::channel::{Channel, ChannelJoin};
use super::deployment::{
    CapacityBinding, DeploymentModel, LeaseBinding, Requirements, RetryPolicy,
};
use super::human_task::TaskStepConfig;
use super::ports::{
    default_automated_input_port, default_automated_output_port, default_initial_port,
    default_join_output_port, default_subworkflow_input_contract, default_subworkflow_output_port,
    default_terminal_port, LoopAccumulator, Port,
};
use super::triggers::{ConcurrencyPolicy, TriggerSource};

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

    // Auto-derived resource/pool requirements manifest (populated on publish,
    // alongside `air_json`). The serialized
    // [`crate::compiler::RequirementsManifest`]: one slot per distinct
    // resource/pool reference the template binds, plus the AIR addresses the
    // home-workspace baseline baked for each. The binding-aware launcher reads
    // this to substitute a different effective resource per instance/workspace
    // without recompiling. NULL on pre-feature rows and on graphs with no
    // resource/pool refs — those launch byte-for-byte as today.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirements_json: Option<serde_json::Value>,

    // Metadata
    pub author_id: Uuid,
    /// `subject_as_uuid()` of whoever last mutated the template (Phase 2).
    /// Backfilled to `author_id` for pre-migration rows; NULL only on a row
    /// written before the migration that somehow had a NULL author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
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

    // --- Library / vendor nodes (migration 20240184) ---
    /// Exclusive intent: `workflow` (default), `library_node` (a curated
    /// reusable integration surfaced in the palette), or `private_child` (a
    /// private sub-workflow). Drives palette/catalogue/ACL branching.
    #[serde(default = "default_template_kind")]
    pub template_kind: String,
    /// Provenance/trust axis for library nodes: `system` (platform-seeded,
    /// read-only) | `workspace` | `community`. NULL for plain workflows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Stable `vendor/slug` coordinate (e.g. `openfoam/solid-displacement`),
    /// unique within `origin`. NULL for plain workflows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinate: Option<String>,
    /// Branding blob (`{icon, color, vendor, category, badge}`) for a library
    /// node; raw JSONB to match the `graph`/`air_json` convention. NULL for
    /// plain workflows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<serde_json::Value>,
    /// Lifecycle of a library node: `active` (default) | `deprecated` |
    /// `retired`. Never gates resolution of an already-pinned embed.
    #[serde(default = "default_lifecycle_status")]
    pub lifecycle_status: String,
    /// Successor `vendor/slug` coordinate for a deprecated/retired node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// Fork provenance (`{coordinate, template_id, version}`) for a workspace
    /// copy forked from an upstream library node. Raw JSONB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<serde_json::Value>,

    /// The caller's effective role (`owner|admin|editor|viewer`) on this
    /// template — annotated by `list_templates`/`get_template` so the SPA can
    /// hide stale edit affordances. Not a DB column (`#[sqlx(default)]` keeps
    /// `FromRow` working); the backend still enforces on every mutate path.
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_effective_role: Option<String>,
}

impl WorkflowTemplate {
    /// Resolve this row's version-chain root: the family `base_template_id`
    /// when set, else `id` for a chain-root row (`COALESCE(base_template_id,
    /// id)`). The canonical way to derive a template's family id.
    pub fn chain_root_id(&self) -> Uuid {
        self.base_template_id.unwrap_or(self.id)
    }
}

impl crate::auth::AclAnnotated for WorkflowTemplate {
    fn acl_id(&self) -> Uuid {
        self.id
    }
    fn set_my_effective_role(&mut self, role: Option<String>) {
        self.my_effective_role = role;
    }
}

/// Lightweight list-row projection of [`WorkflowTemplate`].
///
/// The paginated `GET /api/v1/templates` list returns this instead of the full
/// row. It deliberately OMITS the four heavy JSONB blobs — `graph`, `air_json`,
/// `interface_json`, `source_ref` — which the detail endpoint
/// (`GET /api/v1/templates/{id}`) still serves. Those blobs are each a full
/// workflow graph / compiled AIR; carrying them per-row blew a 20-row page up
/// to ~20 MB. Every remaining field mirrors `WorkflowTemplate` exactly (same
/// names + serde shape), so the summary is a strict subset of the detail DTO
/// and the frontend can consume either interchangeably.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow, ToSchema)]
pub struct WorkflowTemplateSummary {
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

    // Metadata
    pub author_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,

    // Workspace + visibility
    pub workspace_id: Uuid,
    pub visibility: String,
    pub owner_template_id: Option<Uuid>,

    /// Compact I/O preview for list cards: the Start nodes' declared input
    /// field names. Computed server-side from the `graph` column (so the list
    /// needn't ship the whole graph). Skipped when empty — which also keeps the
    /// full [`WorkflowTemplate`] (no such field) assignable to this summary on
    /// the frontend. See `list_templates`.
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub io_inputs: Vec<String>,
    /// Deduped End-node result-mapping target fields — the workflow's declared
    /// outputs. Companion to [`Self::io_inputs`].
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub io_outputs: Vec<String>,

    /// Caller's effective role — annotated by `list_templates`, never a column.
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_effective_role: Option<String>,
}

impl crate::auth::AclAnnotated for WorkflowTemplateSummary {
    fn acl_id(&self) -> Uuid {
        self.id
    }
    fn set_my_effective_role(&mut self, role: Option<String>) {
        self.my_effective_role = role;
    }
}

fn default_template_kind() -> String {
    "workflow".to_string()
}

fn default_lifecycle_status() -> String {
    "active".to_string()
}

/// Branding for a library node — surfaced in the palette and frozen onto an
/// embedding `SubWorkflow` node so the canvas renders a vendor-branded card
/// (decisions 9, 13). `icon` is a key into the frontend icon registry (never
/// raw SVG); `color` is a hex/token accent. Stored as JSONB on the template
/// row; this typed shape feeds the OpenAPI surface (io-contract + node data).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct Presentation {
    /// Icon registry key (e.g. `openfoam`). Falls back to a generic icon when
    /// unknown to the frontend registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Accent color (hex like `#1a73e8`, or a design-token name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Vendor / publisher display name (e.g. `OpenFOAM`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// Palette grouping category (controlled vocab, e.g. `CFD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Optional short badge (e.g. a version tag `v2406`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
}

/// Controlled vocabulary for a library node's palette `category` (decision 6).
/// The category drives the two-level Library palette grouping (category →
/// vendor), so it is constrained to keep that grouping coherent; the `vendor`
/// field stays free text. Validated at seed time and (later) at promote time.
/// Extend this list as new integration domains land — it is intentionally a
/// plain constant, not a DB enum, so adding a category needs no migration.
pub const LIBRARY_CATEGORIES: &[&str] = &[
    "Examples",
    "CFD",
    "FEA",
    "Micromagnetics",
    "Molecular Dynamics",
    "Quantum",
    "Bioinformatics",
    "ML",
    "Robotics",
    "Imaging",
    "Data",
    "Optimization",
    "Simulation",
];

/// Whether `category` is a member of the controlled [`LIBRARY_CATEGORIES`]
/// vocabulary. Match is case-sensitive — the listed casing is canonical.
pub fn is_known_library_category(category: &str) -> bool {
    LIBRARY_CATEGORIES.contains(&category)
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

/// Structural metrics of a published [`WorkflowGraph`], computed once at
/// publish time and persisted into the `workflow_templates.metrics` JSONB
/// column. Pure shape — no run data — so it can be derived from the graph
/// alone, before the template ever runs. The per-template analytics surface
/// reads it back as the "what this template is" half of the view (the
/// "how it ran" half comes from the run/node rollup tables).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TemplateMetrics {
    /// Total nodes in the graph.
    pub node_count: u32,
    /// Total edges in the graph.
    pub edge_count: u32,
    /// Node counts keyed by the snake-case wire kind (`WorkflowNodeData::
    /// type_name()`): `start`, `automated_step`, `decision`, … BTreeMap for a
    /// byte-stable JSONB serialization.
    pub node_kind_counts: std::collections::BTreeMap<String, u32>,
    /// Number of `SubWorkflow` nodes (embedded child templates).
    pub subworkflow_count: u32,
    /// Deepest visual-container nesting in the graph, measured by the
    /// `parent_id` chain (a top-level node is depth 0, a node inside one
    /// container is depth 1, and so on).
    pub max_nesting_depth: u32,
    /// Whether the graph contains any `Loop` (or `Map`) iteration node.
    pub has_loops: bool,
}

impl TemplateMetrics {
    /// Derive the structural metrics from a graph. Counts every node by its
    /// wire kind, tallies SubWorkflow/loop presence, and walks each node's
    /// `parent_id` chain to find the deepest container nesting.
    pub fn from_graph(graph: &WorkflowGraph) -> Self {
        use std::collections::{BTreeMap, HashMap};

        let mut node_kind_counts: BTreeMap<String, u32> = BTreeMap::new();
        let mut subworkflow_count = 0u32;
        let mut has_loops = false;

        for node in &graph.nodes {
            let kind = node.data.type_name();
            *node_kind_counts.entry(kind.to_string()).or_insert(0) += 1;
            match node.data {
                WorkflowNodeData::SubWorkflow { .. } => subworkflow_count += 1,
                WorkflowNodeData::Loop { .. } | WorkflowNodeData::Map { .. } => has_loops = true,
                _ => {}
            }
        }

        // Visual-container nesting depth: follow each node's `parent_id` chain.
        // A `parent_of` lookup keyed by node id lets a node resolve its parent
        // without rescanning; a guard caps the walk at the node count so a
        // (malformed) cycle can't spin forever.
        let parent_of: HashMap<&str, Option<&str>> = graph
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.parent_id.as_deref()))
            .collect();
        let node_count = graph.nodes.len();
        let mut max_nesting_depth = 0u32;
        for node in &graph.nodes {
            let mut depth = 0u32;
            let mut cursor = node.parent_id.as_deref();
            let mut steps = 0usize;
            while let Some(parent) = cursor {
                depth += 1;
                steps += 1;
                if steps > node_count {
                    break;
                }
                cursor = parent_of.get(parent).copied().flatten();
            }
            max_nesting_depth = max_nesting_depth.max(depth);
        }

        Self {
            node_count: node_count as u32,
            edge_count: graph.edges.len() as u32,
            node_kind_counts,
            subworkflow_count,
            max_nesting_depth,
            has_loops,
        }
    }
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
        /// Bind this human task to a Presence `capacity` resource (docs/33/34).
        /// Mirrors [`DeploymentModel::Executor`]'s `capacity`: `None` ⇒ today's
        /// unpooled lowering (byte-identical); `Some` ⇒ the task is *offered* to
        /// eligible available members of the named capacity and lowered as the
        /// pooled claim/acquire/register/release scaffold (consent acceptance —
        /// the offer/claim handshake, doc 35 §4).
        /// Resolved at publish like `AutomatedStep` to the backing
        /// `pool-<capacity_id>` net.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capacity: Option<CapacityBinding>,
        /// Placement Requirements on a capacity-bound human task — typed
        /// [`Constraint`]s over the offered member's advertised `caps`. Mirrors
        /// [`WorkflowNodeData::AutomatedStep`]'s `requirements`: injected into the
        /// offer's claim payload as a Rhai literal, with the offer pool's
        /// `t_claim` guard (`satisfies(offer.requirements, unit.caps)`) admitting
        /// ONLY a member whose caps satisfy every constraint. `None` (the
        /// default) ⇒ no placement constraint (any available member may claim).
        /// Ignored when `capacity` is `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requirements: Option<Requirements>,
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
        /// Statically-declared streaming [`Channel`]s (docs/25). Each control-
        /// output channel synthesizes a place `p_{id}_{name}` the job emits into
        /// at runtime via `emit`/`scatter`; downstream edges wire to it by
        /// `sourceHandle`/`targetHandle == name`. `#[serde(default)]` ⇒ existing
        /// templates (field absent → empty) round-trip unchanged.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        channels: Vec<Channel>,
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
        /// `deployment_model`/`channels`).
        #[serde(
            rename = "assetBindings",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
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
    /// Container that holds ONE unit of capacity for the duration of its body —
    /// EITHER a `datacenter` allocation (a leased salloc/Nomad alloc) OR a
    /// `presence` capacity unit (a single lab runner, claimed exclusively for
    /// the scope). Decouples "hold capacity" from "loop": any AutomatedStep
    /// nested inside a LeaseScope — directly or through intervening containers
    /// like a plain Loop — runs ON the held unit by containment (no per-step
    /// flag). The unit is acquired once on enter and released once on exit; the
    /// held lease (incl. `executor_namespace` — `lease-<grant>` for a datacenter,
    /// `runner.<id>` for a presence runner) is parked into the scope's
    /// `p_<id>_data` envelope under a `lease` key, so body steps and downstream
    /// blocks borrow `<scope_slug>.lease.<field>` through the standard read-arc
    /// pipeline. A nested plain `Executor` body step inherits the held
    /// `executor_namespace` by containment, so its job lands on the SAME held
    /// runner / warm drain executor. Children attach via the same
    /// `body_in`/`body_out` interior handles as Loop (`parent_id ==
    /// lease_scope.id`); the perimeter `in`/`out` handles connect to the outer
    /// flow.
    ///
    /// To hold ONE unit across loop iterations, compose `LeaseScope { Loop { … } }`
    /// — the scope acquires before the loop starts and releases after it exits.
    #[serde(rename = "lease_scope")]
    LeaseScope {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// REQUIRED capacity lease binding (a LeaseScope with no lease is a
        /// pointless empty container). Reuses [`LeaseBinding`] — the `pool` alias
        /// resolves via `resolve_binding(..., LeaseHolder, ...)` to a `datacenter`
        /// (Scheduler backend) OR a `presence` `capacity` (Presence backend) — and
        /// is NOT `Option`; `validate_lease_scope` rejects an empty `pool` alias.
        lease: LeaseBinding,
        /// Placement Requirements for a PRESENCE-backed lease (the scope picks
        /// WHICH runner to hold). A set of typed [`Constraint`]s over the runner
        /// unit's advertised `caps`; the compiler injects them into the claim and
        /// the presence pool's `t_grant` guard
        /// (`satisfies(claim.requirements, unit.caps)`) admits only a runner whose
        /// caps satisfy every constraint. `None` (the default) ⇒ no constraint
        /// (matches any unit). Ignored for a `datacenter` lease (the `request`
        /// shapes the alloc there). Body steps do NOT re-match — they inherit the
        /// held runner by containment. `#[serde(default, skip_serializing_if)]` ⇒
        /// existing templates round-trip unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requirements: Option<Requirements>,
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
        /// Repeater items-ref machinery), e.g. `extract.tasks`.
        #[serde(rename = "itemsRef", default)]
        items_ref: String,
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
        /// Node-level asset bindings (docs/20 §5, feature B). When the Map's
        /// `itemsRef` is a bare identifier matching one of these aliases (and
        /// is NOT a producer slug — producer wins), the scatter draws its
        /// source collection from the bound asset's records via the publish-time
        /// `let __assets = #{...}` splice (`let __src = __assets["<alias>"]`),
        /// instead of from an upstream producer read-arc. `#[serde(default)]` ⇒
        /// existing templates (field absent → empty) round-trip unchanged (same
        /// precedent as AutomatedStep's `asset_bindings`).
        #[serde(
            rename = "assetBindings",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        asset_bindings: Vec<AssetBinding>,
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
        #[serde(
            rename = "assetBindings",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
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
        /// Stable `vendor/slug` coordinate of the child when it is a library
        /// node (decision 7). Frozen onto the node by the editor's io-contract
        /// fetch alongside `input_contract`/`output`; lets the canvas brand the
        /// card and the upgrade prompt track the source. Absent ⇒ a plain
        /// (non-library) sub-workflow embed.
        #[serde(
            rename = "sourceCoordinate",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        source_coordinate: Option<String>,
        /// Frozen branding snapshot copied from the library-node child
        /// (decisions 9, 12). Display-only — never feeds compilation. Refreshed
        /// like `input_contract` from the io-contract fetch. Absent ⇒ render the
        /// generic sub-workflow card.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        presentation: Option<Presentation>,
    },
    /// Workflow-as-streaming-endpoint INGRESS (docs/25 §9 Phase 3). Declares
    /// `Out` [`Channel`]s an external producer feeds through a mekhan ingress
    /// endpoint; each declared channel surfaces as a named source handle the
    /// graph wires downstream consumers off, exactly like an
    /// [`WorkflowNodeData::AutomatedStep`]'s `Out` channels. A StreamSource
    /// has NO control-flow handles in v1 — no inbound control edge, no
    /// default `out`; its only handles are its channel handles. Lowering
    /// synthesizes the standard per-channel place `p_{id}_{name}` (WI-2).
    #[serde(rename = "stream_source")]
    StreamSource {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Declared streaming [`Channel`]s. Direction is expected to be `Out`
        /// for every entry (the node *produces* into the net); enforced by
        /// validation (WI-2), not the type. `#[serde(default)]` ⇒ existing
        /// templates (field absent → empty) round-trip unchanged.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        channels: Vec<Channel>,
    },
    /// Workflow-as-streaming-endpoint EGRESS (docs/25 §9 Phase 3). Declares
    /// exactly ONE `In` [`Channel`] (enforced by validation in WI-2, not the
    /// type — the field stays `Vec<Channel>` so the three channel-bearing
    /// variants share one accessor shape, see
    /// [`WorkflowNodeData::channels`]). The upstream producer edge wires to
    /// the channel's named target handle; mekhan exposes the sunk stream on
    /// an egress endpoint. No control-flow handles in v1.
    #[serde(rename = "stream_sink")]
    StreamSink {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Declared streaming [`Channel`]s — exactly one `In` entry in v1
        /// (validation-enforced, WI-2). `#[serde(default)]` ⇒ absent → empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        channels: Vec<Channel>,
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
            | Self::PhaseUpdate { label, .. }
            | Self::ProgressUpdate { label, .. }
            | Self::Failure { label, .. }
            | Self::Delay { label, .. }
            | Self::Timeout { label, .. }
            | Self::Trigger { label, .. }
            | Self::SubWorkflow { label, .. }
            | Self::StreamSource { label, .. }
            | Self::StreamSink { label, .. } => label,
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
            | Self::PhaseUpdate { description, .. }
            | Self::ProgressUpdate { description, .. }
            | Self::Failure { description, .. }
            | Self::Delay { description, .. }
            | Self::Timeout { description, .. }
            | Self::Trigger { description, .. }
            | Self::SubWorkflow { description, .. }
            | Self::StreamSource { description, .. }
            | Self::StreamSink { description, .. } => description.as_deref(),
        }
    }

    /// The statically-declared streaming [`Channel`]s this variant carries.
    /// The single accessor validation + lowering dispatch through so the
    /// three channel-bearing variants (`AutomatedStep`, `StreamSource`,
    /// `StreamSink`) can't drift; every other variant returns the empty
    /// slice.
    pub fn channels(&self) -> &[Channel] {
        match self {
            Self::AutomatedStep { channels, .. }
            | Self::StreamSource { channels, .. }
            | Self::StreamSink { channels, .. } => channels,
            _ => &[],
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

/// Default `Map.item_var` — body tokens bind the per-element value as `item`.
fn default_item_var() -> String {
    "item".to_string()
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
    /// CONSUMER-side fold discipline for a CONTROL channel edge (see
    /// [`ChannelJoin`]). Applies only when this edge's `target_handle` names a
    /// control IN channel fed by a control OUT channel: `each` fires the
    /// downstream once per emitted item, `gather` collects the whole episode
    /// into one array at a counted barrier. Absent/`None` ⇒ `Each`. Must NOT
    /// be set on data-plane edges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join: Option<ChannelJoin>,
    #[serde(rename = "type")]
    pub edge_type: String,
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
                join: None,
                edge_type: "sequence".to_string(),
            }],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_category_vocab_is_case_sensitive_and_covers_seeded_packs() {
        // The two system packs seeded today must validate.
        assert!(is_known_library_category("CFD"));
        assert!(is_known_library_category("Examples"));
        // Case-sensitive: the listed casing is canonical.
        assert!(!is_known_library_category("cfd"));
        assert!(!is_known_library_category("examples"));
        // Unknown is rejected.
        assert!(!is_known_library_category("Frobnication"));
        // The vocab is non-empty and free of duplicates.
        assert!(!LIBRARY_CATEGORIES.is_empty());
        let mut seen = std::collections::HashSet::new();
        for c in LIBRARY_CATEGORIES {
            assert!(
                seen.insert(*c),
                "duplicate category in LIBRARY_CATEGORIES: {c}"
            );
        }
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
                capacity: None,
                requirements: None,
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
}
