//! # Sub-graph interface registry — the compiler's typed contract
//!
//! Every workflow node lowers into a known Petri sub-graph. The shape of that
//! sub-graph (where tokens enter, where they leave, which place parks the
//! borrowable data envelope, which place is a workflow exit) is **explicit
//! output** of lowering, recorded in [`InterfaceRegistry`]. Downstream passes
//! consume the registry — they never pattern-match on place id conventions
//! or `place_type` to recover boundary information.
//!
//! ## Mental model
//!
//! ```text
//!   WorkflowNode               Sub-graph (Petri places + transitions)
//!   ─────────────              ────────────────────────────────────────
//!   Start { ... }       →      p_<id>_ready (entry)
//!                              p_<id>_data  (write-once parked envelope)
//!                              p_<id>_main  (default output)
//!                              t_<id>_park  (fork: data + main)
//!
//!   HumanTask { ... }   →      p_<id>_input   (entry)
//!                              p_<id>_active  (state)
//!                              p_<id>_data    (parked envelope)
//!                              p_<id>_ctrl    (default output, slimmed control token)
//!                              ...
//!
//!   End { ... }         →      p_<id>_done       (entry)
//!                              p_<id>_result     (workflow terminal — if result_mapping)
//!                              p_<id>_completed  (workflow terminal — if process registered)
//! ```
//!
//! After lowering, the dispatcher walks every node and asserts that a
//! [`NodeInterface`] entry was published. Then `compile.rs`:
//!
//!   1. Builds the alias map from pass-through edge merges (`apply_merges`).
//!   2. Calls `derive_node_ownership` — the ONE prefix-match pass that fills
//!      `owned_places` / `owned_transitions` from `p_<id>_*` / `t_<id>_*`.
//!   3. Rewrites every place id in every interface through the alias map, so
//!      consumers see post-collapse ids.
//!
//! From that point onward, the registry is the source of truth.
//!
//! ## Contract every `lower_*` must satisfy
//!
//! The dispatcher hard-errors if a lowering returns `Ok` without publishing
//! an interface entry. Each lowering populates:
//!
//! | Field                 | When set                                                                  |
//! |-----------------------|---------------------------------------------------------------------------|
//! | `node_id`             | always                                                                    |
//! | `kind`                | always (mirrors `WorkflowNodeData` variant)                               |
//! | `entry`               | `Some` for nodes with an inbound boundary                                 |
//! | `named_inputs`        | every named inbound port (Loop `body_out`, Join per-edge inputs)         |
//! | `outputs`             | every outbound port keyed by [`OutputKey`]                                |
//! | `data_port`           | `Some` iff the node parks a borrow-reachable envelope (Start/HumanTask/AutomatedStep) |
//! | `workflow_terminals`  | `End` nodes only — every terminal place this End feeds                    |
//! | `owned_places`        | filled centrally by `derive_node_ownership` (do NOT set in `lower_*`)     |
//! | `owned_transitions`   | filled centrally by `derive_node_ownership` (do NOT set in `lower_*`)     |
//!
//! `Trigger` is the sole exception: it has no AIR shape, no interface entry.
//!
//! ## Ownership invariant
//!
//! Every place a `lower_*` emits MUST start with `p_{node_id}_`, every
//! transition with `t_{node_id}_`. This is the ONLY place-id convention the
//! compiler enforces; `derive_node_ownership` does longest-prefix matching to
//! credit ownership. Any lowering that violates the convention will silently
//! lose ownership of those places (they won't be scope-tagged, won't appear
//! in `owned_places`).
//!
//! ## Alias collapse
//!
//! Pure-passthrough edge wiring is optimized into a place merge: the
//! consumer's input place gets aliased onto the producer's output place.
//! `NodeInterface::rewrite_places` is run once on every interface after
//! `resolve_aliases` so every field carries post-collapse ids. This is the
//! structural fix for the bugs that motivated this design — consumers don't
//! need to know aliases happened.
//!
//! ## What downstream passes read
//!
//! - `compile.rs` step 7 (terminal `place_type` fixup): reads
//!   `interface.workflow_terminals` only.
//! - `compile.rs` step 8b (scope-child group tagging): reads
//!   `interface.owned_places` / `owned_transitions`.
//! - `compile.rs` step 10 (data-port schema binding + read-arc synthesis):
//!   reads `interface.data_port`.
//! - `publish.rs::resolve_subworkflow_air` (parent compile embedding a
//!   `SubWorkflow` child): reads the child's published `interface_json` —
//!   `entry` of the unique `Start` + `workflow_terminals` union over all
//!   `End` nodes.
//!
//! No consumer touches `place_type`, prefix-matches place ids, or filters
//! `<step>/<state>` slash-shapes. If you find yourself doing that in a new
//! pass, extend the interface instead.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::str::FromStr;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

/// Discriminates which lowering variant produced an interface. Mirrors
/// `WorkflowNodeData` so consumers can dispatch without re-inspecting the
/// graph. (CatalogueQuery is *not* a top-level variant — it's an AutomatedStep
/// flavour selected by `execution_spec.backend_type`.)
///
/// `Agent` is its own variant: the loop path emits the agent-specific subnet
/// (`p_state`, `p_response`, `p_final`, `t_route_final`, …), but the parked
/// output envelope has the SAME `{detail: {outputs: …}}` nesting an
/// `AutomatedStep(Llm)` produces — so `hoist_path` returns `["detail",
/// "outputs"]` for both. The degenerate path delegates to the AutomatedStep
/// lowering via a virtual node, so the published interface kind for that
/// path stays `AutomatedStep` and the byte-identical contract holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Start,
    End,
    HumanTask,
    AutomatedStep,
    Agent,
    Decision,
    Loop,
    LeaseScope,
    ParallelSplit,
    Join,
    Scope,
    Map,
    SubWorkflow,
    PhaseUpdate,
    ProgressUpdate,
    Failure,
    Delay,
    Timeout,
    Trigger,
}

impl NodeKind {
    /// Unquoted hoist-path segments for this kind's parked-envelope shape.
    /// HumanTask nests business data under `data`; AutomatedStep + Agent
    /// (which share the executor envelope shape) nest under `detail.outputs`;
    /// every other kind keeps fields at the top level. The borrow apply
    /// phases use this to bridge the user-visible flat shape
    /// (`<slug>.<field>`) to the nested engine shape.
    pub fn hoist_path(&self) -> &'static [&'static str] {
        match self {
            NodeKind::HumanTask => &["data"],
            NodeKind::AutomatedStep | NodeKind::Agent => &["detail", "outputs"],
            _ => &[],
        }
    }

    /// Snake-case wire string for this kind. Used by the step-executions
    /// projection writer and `NodeDescriptor` serialization (consumed by
    /// `GET /api/v1/node-types`).
    pub fn wire_str(&self) -> &'static str {
        match self {
            NodeKind::Start => "start",
            NodeKind::End => "end",
            NodeKind::HumanTask => "human_task",
            NodeKind::AutomatedStep => "automated_step",
            NodeKind::Agent => "agent",
            NodeKind::Decision => "decision",
            NodeKind::Loop => "loop",
            NodeKind::LeaseScope => "lease_scope",
            NodeKind::ParallelSplit => "parallel_split",
            NodeKind::Join => "join",
            NodeKind::Scope => "scope",
            NodeKind::Map => "map",
            NodeKind::SubWorkflow => "sub_workflow",
            NodeKind::PhaseUpdate => "phase_update",
            NodeKind::ProgressUpdate => "progress_update",
            NodeKind::Failure => "failure",
            NodeKind::Delay => "delay",
            NodeKind::Timeout => "timeout",
            NodeKind::Trigger => "trigger",
        }
    }
}

/// How an output port is keyed. Mirrors `NodePorts.output_places` (which uses
/// `Vec<(Option<String>, PlaceHandle)>`) but lifts the meaning into named
/// variants so consumers don't have to guess what `Some("branch_1")` means
/// vs `Some("e_42")`.
///
/// Serializes as a flat string so `BTreeMap<OutputKey, _>` is JSON-object-safe
/// (JSON requires string keys; the default derived enum would emit `{"Edge":
/// "e1"}` which `serde_json` refuses as a map key). Wire shape:
/// `"default"` | `"edge:<id>"` | `"named:<id>"`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OutputKey {
    /// Single-output nodes: Start.main, HumanTask.out, AutomatedStep.success,
    /// End/Failure (no outputs — never present), CatalogueQuery.out, ...
    Default,
    /// ParallelSplit per-edge fanout. (Join per-edge inputs are recorded in
    /// `named_inputs`, not here.)
    Edge(String),
    /// Decision branches, Loop body/exit, AutomatedStep success/error pair.
    Named(String),
}

impl fmt::Display for OutputKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OutputKey::Default => f.write_str("default"),
            OutputKey::Edge(s) => write!(f, "edge:{s}"),
            OutputKey::Named(s) => write!(f, "named:{s}"),
        }
    }
}

impl FromStr for OutputKey {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "default" {
            Ok(OutputKey::Default)
        } else if let Some(rest) = s.strip_prefix("edge:") {
            Ok(OutputKey::Edge(rest.to_string()))
        } else if let Some(rest) = s.strip_prefix("named:") {
            Ok(OutputKey::Named(rest.to_string()))
        } else {
            Err(format!("unknown OutputKey: {s}"))
        }
    }
}

impl Serialize for OutputKey {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for OutputKey {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        s.parse().map_err(de::Error::custom)
    }
}

/// The explicit shape a single lowered node exposes to the rest of the
/// compiler. Recorded in the registry post-lowering, alias-rewritten once
/// after `apply_merges`, then consumed read-only by downstream passes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeInterface {
    pub node_id: String,
    pub kind: NodeKind,

    // ── Boundary ────────────────────────────────────────────────────────────
    /// Single inbound place tokens enter the sub-graph through. `None` for
    /// Trigger (pre-compile dispatcher concern; no AIR shape).
    pub entry: Option<String>,
    /// Named inbound ports: Loop's `body_out` (where the body's outgoing
    /// edge lands), Join per-edge inputs (keyed by edge id), ...
    pub named_inputs: BTreeMap<String, String>,
    /// Outbound places keyed by their port semantics. See `OutputKey`.
    pub outputs: BTreeMap<OutputKey, String>,

    // ── Borrow surface ──────────────────────────────────────────────────────
    /// Some(place_id) iff this node parks a write-once data envelope (today:
    /// Start via `park_outputs`, HumanTask/AutomatedStep via `split_outputs`).
    /// The single place every `<slug>.<field>` reference read-arcs against.
    pub data_port: Option<String>,

    // ── Lifecycle ───────────────────────────────────────────────────────────
    /// End-derived workflow-exit terminals after alias resolution. Empty for
    /// non-End nodes. Distinct from SDK executor-lifecycle "terminal" places
    /// (`<step>/completed` etc.) — those are NOT recorded here, which is the
    /// entire point: consumers stop having to disambiguate via slash-exclusion.
    pub workflow_terminals: Vec<String>,

    // ── Sub-graph membership ────────────────────────────────────────────────
    /// Every place this node owns (post-alias-resolution). Replaces the
    /// `p_{node_id}_` prefix match in `compile.rs` step 8b.
    pub owned_places: Vec<String>,
    /// Every transition this node owns (post-alias-resolution). Replaces the
    /// `t_{node_id}_` prefix match in `compile.rs` step 8b.
    pub owned_transitions: Vec<String>,

    // ── Author-visible borrows ──────────────────────────────────────────────
    /// `producer_node_id → [attr, …]` — the first-segment fields this node's
    /// author references off each upstream parked envelope. Populated at
    /// compile time from Python source (`extract_python_refs`) for
    /// AutomatedSteps and from `{{ <slug>.<attr> }}` placeholders for
    /// HumanTasks. The frontend renders these alongside the runtime inputs
    /// so the user can see *what the step actually read* — not just the
    /// full upstream envelope handed over the edge.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub borrowed_paths: BTreeMap<String, Vec<String>>,

    // ── Cancellable in-flight state ─────────────────────────────────────────
    /// Populated by `lower_*` for nodes whose lowering parks a long-running
    /// correlation token in a known place (HumanTask, Executor-backed
    /// AutomatedStep, SubWorkflow, Delay). When the node ends up inside a
    /// Timeout body, the Timeout's post-pass reads this and synthesizes a
    /// drain transition + matching `<kind>_cancel` effect transition so the
    /// in-flight resource is reclaimed when the timer wins.
    ///
    /// Non-cancellable kinds (Decision, ParallelSplit, Join, Scope,
    /// PhaseUpdate, ProgressUpdate, Failure, Start, End, Trigger, Timeout)
    /// leave this `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellable: Option<CancellableInFlight>,
}

/// Kind tag for a cancellable in-flight resource — drives Timeout's drain
/// post-pass to fire the matching `<kind>_cancel` engine effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelKind {
    /// HumanTask: drains via `human_cancel` (`task_id` + `place`).
    Human,
    /// AutomatedStep (executor backend): drains via `executor_cancel`
    /// (`execution_id`).
    Executor,
    /// AutomatedStep (scheduler backend): drains via `scheduler_cancel`
    /// (`scheduler_job_id`).
    Scheduler,
    /// Delay / nested Timeout's timer: drains via `timer_cancel`
    /// (`timer_correlation_id` + `target_place_id`).
    Timer,
    /// SubWorkflow: drains via `subworkflow_cancel` (`child_net_id`).
    SubWorkflow,
}

/// Where a node's "currently in-flight" correlation token lives, what kind
/// of cancel effect drains it, and which token field carries the id the
/// cancel handler expects. The Timeout post-pass synthesizes one drain
/// transition per cancellable body child using this metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CancellableInFlight {
    /// Place id where the node parks its in-flight correlation token.
    pub place_id: String,
    pub kind: CancelKind,
    /// Field name on the in-flight token carrying the cancel correlation
    /// id (`task_id`, `execution_id`, `scheduler_job_id`,
    /// `timer_correlation_id`, `child_net_id`).
    pub correlation_field: String,
    /// Additional field the cancel handler requires beyond the correlation
    /// id. Today only `Timer` uses this (carries `target_place_id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_field: Option<String>,
}

impl NodeInterface {
    pub fn new(node_id: impl Into<String>, kind: NodeKind) -> Self {
        Self {
            node_id: node_id.into(),
            kind,
            entry: None,
            named_inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
            data_port: None,
            workflow_terminals: Vec::new(),
            owned_places: Vec::new(),
            owned_transitions: Vec::new(),
            borrowed_paths: BTreeMap::new(),
            cancellable: None,
        }
    }

    /// Default-port output (Start, HumanTask, AutomatedStep, CatalogueQuery,
    /// PhaseUpdate, ProgressUpdate, SubWorkflow). One-liner for the common case.
    pub fn with_default_output(mut self, place: impl Into<String>) -> Self {
        self.outputs.insert(OutputKey::Default, place.into());
        self
    }

    /// Look up an output by raw `(Option<String>)` key the way `NodePorts`
    /// stores them — `None` → `Default`, `Some(s)` → `Edge(s)` (the caller
    /// already knows whether it's an edge id or a branch name). Returns
    /// `Some(&place_id)` if present.
    pub fn output_by_legacy_key(&self, key: Option<&str>) -> Option<&str> {
        let k = match key {
            None => OutputKey::Default,
            Some(s) => OutputKey::Edge(s.to_string()),
        };
        if let Some(p) = self.outputs.get(&k) {
            return Some(p.as_str());
        }
        // Fall back: named outputs may have been recorded as Named(s) (e.g.,
        // success/error/branch_<key>) — try that flavor too.
        if let Some(s) = key {
            return self
                .outputs
                .get(&OutputKey::Named(s.to_string()))
                .map(String::as_str);
        }
        None
    }

    /// Rewrite every place id through the alias map. Called once between
    /// `resolve_aliases` and the first consumer in `compile.rs`.
    pub fn rewrite_places(&mut self, alias: &HashMap<String, String>) {
        rewrite(&mut self.entry, alias);
        for v in self.named_inputs.values_mut() {
            *v = alias.get(v.as_str()).cloned().unwrap_or_else(|| v.clone());
        }
        for v in self.outputs.values_mut() {
            *v = alias.get(v.as_str()).cloned().unwrap_or_else(|| v.clone());
        }
        if let Some(d) = self.data_port.as_mut() {
            *d = alias.get(d.as_str()).cloned().unwrap_or_else(|| d.clone());
        }
        for t in self.workflow_terminals.iter_mut() {
            *t = alias.get(t.as_str()).cloned().unwrap_or_else(|| t.clone());
        }
        // Owned places: rewrite, then drop entries that aliased into another
        // node's place (the survivor lives in that node's owned_places, not
        // ours). Dedup since alias collapse can merge two own-places into one.
        let mut seen = std::collections::HashSet::new();
        self.owned_places.retain_mut(|p| {
            if let Some(s) = alias.get(p.as_str()) {
                *p = s.clone();
            }
            seen.insert(p.clone())
        });
    }
}

fn rewrite(slot: &mut Option<String>, alias: &HashMap<String, String>) {
    if let Some(v) = slot.as_mut() {
        if let Some(s) = alias.get(v.as_str()) {
            *v = s.clone();
        }
    }
}

/// node_id → interface. `HashMap` rather than `BTreeMap` because consumers
/// look up by node id, never iterate ordered.
pub type InterfaceRegistry = HashMap<String, NodeInterface>;
