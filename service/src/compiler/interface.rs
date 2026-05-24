//! Per-node compiler interface: the explicit shape every `lower_*` already
//! emits (sub-graph boundary places, terminal exits, parked data port, owned
//! places/transitions) — recorded on a registry instead of left for downstream
//! passes to rediscover via `format!("p_{id}_*")` + `starts_with`.
//!
//! The registry is the seam where the type-driven lowering layer
//! (`token_shape::SlugIndex`, per-node `lower_*`) meets the cross-cutting
//! passes (scope-child tagging, schema binding, sub-workflow reply wiring,
//! read-arc synthesis). Today every leak we've fixed
//! (`e5ed9fc`/`674408e` SubWorkflow terminal filter, `cd1825f` Loop counter,
//! priority tiebreaker fallback) sat on that seam.
//!
//! Scope: this prototype adds the registry + a single alias-rewrite pass +
//! consumption at three demonstration sites (`publish.rs::resolve_subworkflow_air`,
//! `compile.rs` step 8b scope tagging, step 10 data-port lookup). Each
//! `lower_*` still constructs ids via `format!` — production-side naming is
//! fine; the leak is on the read side.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

/// Discriminates which lowering variant produced an interface. Mirrors
/// `WorkflowNodeData` so consumers can dispatch without re-inspecting the
/// graph. (CatalogueQuery is *not* a top-level variant — it's an AutomatedStep
/// flavour selected by `execution_spec.backend_type`.)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeKind {
    Start,
    End,
    HumanTask,
    AutomatedStep,
    Decision,
    Loop,
    ParallelSplit,
    ParallelJoin,
    Scope,
    SubWorkflow,
    PhaseUpdate,
    ProgressUpdate,
    Failure,
    Trigger,
}

/// How an output port is keyed. Mirrors `NodePorts.output_places` (which uses
/// `Vec<(Option<String>, PlaceHandle)>`) but lifts the meaning into named
/// variants so consumers don't have to guess what `Some("branch_1")` means
/// vs `Some("e_42")`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum OutputKey {
    /// Single-output nodes: Start.main, HumanTask.out, AutomatedStep.success,
    /// End/Failure (no outputs — never present), CatalogueQuery.out, ...
    Default,
    /// ParallelSplit per-edge fanout, ParallelJoin (per-edge input is recorded
    /// in `named_inputs`, not here).
    Edge(String),
    /// Decision branches, Loop body/exit, AutomatedStep success/error pair.
    Named(String),
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
    /// edge lands), ParallelJoin per-edge inputs (keyed by edge id), ...
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
