//! Pure fold from the engine event log + `NodeInterface` registry into
//! per-step execution rows. Used by both the consumer (online ingest) and
//! tests (offline replay).
//!
//! ## Attribution model
//!
//! The compiler's `NodeInterface` registry (see
//! `service/src/compiler/interface.rs`) gives us, for every workflow node:
//! - `entry` — the boundary place tokens arrive at
//! - `data_port` — the parked envelope place (Start/HumanTask/AutomatedStep/Loop/SubWorkflow)
//! - `outputs` — keyed by `OutputKey::{Default,Edge,Named}`
//! - `workflow_terminals` — End-derived workflow-exit places
//! - `owned_transitions` / `owned_places` — full sub-graph membership
//!
//! Build reverse-indexes once (`transition_id → node_id`, `place_id →
//! node_id`), then fold the event stream:
//!
//! - **Iteration arrival**: a token landing at node N's `entry` creates a
//!   new `(node_id, iteration_index)` row at status `Pending`. Loop body
//!   nodes get one row per body iteration; non-loop nodes always
//!   `iteration_index = 0`.
//! - **Execution start**: the first `TransitionFired` / `EffectCompleted`
//!   whose `transition_id` is owned by N flips status to `Running` and
//!   captures `started_at` + the active iteration's `read_tokens` as
//!   inputs (grouped by producer node via `owned_places` reverse-index).
//! - **Output capture**: when a fire deposits at N's `data_port` or
//!   `workflow_terminals[*]`, capture the envelope and finalize the row at
//!   `Completed`. For Decision nodes (no `data_port`), `branch_taken` is
//!   the `OutputKey::Edge(edge_id)` of the output that received the token.
//! - **Failure**: `EffectFailed` on N's transition → `Failed` with the
//!   error payload. Also: a token deposit at a parking-style node's
//!   named `"error"` output (AutomatedStep / SubWorkflow retry-exhausted
//!   path) → `Failed` with the error token captured. The `data_port`
//!   stays empty on that path, so without this the row would be stuck
//!   at `Running` until net termination.
//! - **Skipped**: on the terminal lifecycle event (`NetCompleted` /
//!   `NetFailed` / `NetCancelled`), any node without a row gets one at
//!   `Skipped`.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use petri_domain::{
    DomainEvent, PersistedEvent, PlaceId, Token, TokenColor, TokenId, TransitionId,
};

use crate::compiler::{InterfaceRegistry, NodeInterface, NodeKind, OutputKey};

/// Lifecycle of a step in one execution.
///
/// This is the canonical executor-domain phase vocabulary
/// (`aithericon_executor_domain::PhaseStatus`) — the same 5 variants with
/// identical `snake_case` wire form (`"pending"`, `"running"`, ...). The
/// `step_execution.status` text column stores these verbatim. Aliased here so
/// the projection's `StepStatus::Variant` call sites stay unchanged while the
/// type is single-sourced from the wire contract.
pub use aithericon_executor_domain::PhaseStatus as StepStatus;

/// One projected step execution. Keyed by `(node_id, iteration_index)` per
/// instance. Persisted into the `step_execution` table by the consumer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StepExecutionRow {
    pub node_id: String,
    pub iteration_index: i32,
    pub node_kind: NodeKind,
    pub status: StepStatus,
    /// `{ "<producer_node_id>": <envelope>, ... }` grouped by the upstream
    /// node that owns each read-arc place. Absent if the node consumed no
    /// read-arcs.
    pub inputs: Option<serde_json::Value>,
    /// The envelope deposited at the node's `data_port` (parking nodes) or
    /// `workflow_terminals[*]` (End nodes). Absent for nodes that completed
    /// via a control-flow output (Decision, ParallelSplit, ...).
    pub outputs: Option<serde_json::Value>,
    /// The executor `execution_id` (`mekhan-{net}-{uuid}`) hoisted off the
    /// AutomatedStep/Agent envelope before `outputs` is unwrapped to its
    /// business fields. It's the key the datastream tap
    /// (`/api/v1/executions/{execution_id}/channels/{c}/data`) scopes a
    /// channel's out-of-band bytes by, so the UI can play data channels. None
    /// for nodes that aren't executor jobs (Start/End/Decision/...).
    pub execution_id: Option<String>,
    /// For Decision/branching nodes: the `OutputKey` of the output that
    /// received the token, rendered as its wire form (`"edge:<id>"`,
    /// `"named:<id>"`).
    pub branch_taken: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// `EffectFailed` payload (`error_message`, `retryable`, ...) for
    /// failed steps. Absent otherwise.
    pub error: Option<serde_json::Value>,
    /// Engine event sequence number of the last event folded into this row.
    pub last_sequence: u64,
}

/// Project an event stream + the template's compiler interface registry
/// into per-step execution rows.
///
/// Pure: identical input → identical output. Safe to call repeatedly during
/// online ingest (the consumer re-folds the per-instance event buffer on
/// each new event).
pub fn project_step_executions(
    events: &[PersistedEvent],
    registry: &InterfaceRegistry,
) -> Vec<StepExecutionRow> {
    let lookups = Lookups::build(registry);
    let mut state = State::new();

    for ev in events {
        state.absorb(ev, &lookups);
    }

    // Post-fold terminalization, in two passes over the FINAL row set:
    //   1. `close_open_rows` — any row left `Pending`/`Running` at end of the
    //      fold is closed with the terminal outcome (Skipped / Failed). Runs
    //      after the whole stream so it catches rows opened by stray
    //      post-terminal events too.
    //   2. `finalize_unreached` — registry nodes that never got a row at all
    //      are recorded as `Skipped`.
    // Order matters only in that both are gated on `terminated`; they touch
    // disjoint row sets (open existing rows vs. nonexistent ones).
    state.close_open_rows();
    state.finalize_unreached(registry);
    state.into_rows()
}

// ── Reverse-indexes built once from the registry ────────────────────────────

struct Lookups<'a> {
    /// `transition_id → node_id`. Built from each node's `owned_transitions`.
    transition_owner: HashMap<String, String>,
    /// `place_id → node_id`. Built from each node's `owned_places` plus
    /// the boundary places (`entry`, `data_port`, `outputs`,
    /// `workflow_terminals`) to credit ownership of alias-collapsed
    /// boundaries to the node that owns them semantically.
    place_owner: HashMap<String, String>,
    /// `entry_place_id → node_id`. Used to detect token arrivals that open
    /// new iterations.
    entry_to_node: HashMap<String, String>,
    /// `data_port_place_id → node_id`. Used to identify "the parked output
    /// was just deposited" events.
    data_port_to_node: HashMap<String, String>,
    /// `output_place_id → (node_id, OutputKey)`. For Decision-style nodes
    /// where the completed output is identified by which branch place
    /// received the token.
    output_to_node_branch: HashMap<String, (String, OutputKey)>,
    /// `workflow_terminal_place_id → node_id`. End nodes only.
    workflow_terminal_to_node: HashMap<String, String>,
    /// Borrowed reference to the registry for kind lookups during finalize.
    registry: &'a InterfaceRegistry,
}

impl<'a> Lookups<'a> {
    fn build(registry: &'a InterfaceRegistry) -> Self {
        let mut transition_owner = HashMap::new();
        let mut place_owner = HashMap::new();
        let mut entry_to_node = HashMap::new();
        let mut data_port_to_node = HashMap::new();
        let mut output_to_node_branch = HashMap::new();
        let mut workflow_terminal_to_node = HashMap::new();

        for iface in registry.values() {
            for t in &iface.owned_transitions {
                transition_owner.insert(t.clone(), iface.node_id.clone());
            }
            for p in &iface.owned_places {
                place_owner.insert(p.clone(), iface.node_id.clone());
            }
            if let Some(entry) = &iface.entry {
                entry_to_node.insert(entry.clone(), iface.node_id.clone());
                place_owner
                    .entry(entry.clone())
                    .or_insert_with(|| iface.node_id.clone());
            }
            if let Some(dp) = &iface.data_port {
                data_port_to_node.insert(dp.clone(), iface.node_id.clone());
                place_owner
                    .entry(dp.clone())
                    .or_insert_with(|| iface.node_id.clone());
            }
            for (key, place) in &iface.outputs {
                output_to_node_branch.insert(place.clone(), (iface.node_id.clone(), key.clone()));
            }
            for t in &iface.workflow_terminals {
                workflow_terminal_to_node.insert(t.clone(), iface.node_id.clone());
            }
        }

        Self {
            transition_owner,
            place_owner,
            entry_to_node,
            data_port_to_node,
            output_to_node_branch,
            workflow_terminal_to_node,
            registry,
        }
    }

    fn kind_of(&self, node_id: &str) -> NodeKind {
        self.registry
            .get(node_id)
            .map(|i| i.kind)
            // Fallback only ever exercised if a node is referenced from a
            // reverse-index without being in the registry — which is
            // impossible by construction.
            .unwrap_or(NodeKind::AutomatedStep)
    }

    fn interface(&self, node_id: &str) -> Option<&NodeInterface> {
        self.registry.get(node_id)
    }
}

// ── Per-node row state during the fold ──────────────────────────────────────

#[derive(Default)]
struct State {
    /// `(node_id, iteration_index) → row`. Built up as events flow in.
    rows: BTreeMap<(String, i32), StepExecutionRow>,
    /// `node_id → next iteration index to assign`. Incremented when a fresh
    /// token lands at the node's entry place.
    next_iter: HashMap<String, i32>,
    /// `node_id → active iteration index`. The "iteration currently being
    /// projected" for this node. Used to attribute owned-transition fires.
    active_iter: HashMap<String, i32>,
    /// `node_id → set of token ids that have arrived at this node's entry`.
    /// Deduplicates entry arrivals (each unique TokenId opens at most one
    /// iteration row).
    seen_entry_tokens: HashMap<String, HashSet<TokenId>>,
    /// True once a `NetCompleted`/`NetCancelled`/`NetFailed` was seen.
    terminated: bool,
    /// The `(status, timestamp)` to terminalize still-open rows with, captured
    /// from the terminal lifecycle event. `NetFailed → Failed`;
    /// `NetCompleted`/`NetCancelled → Skipped`. Applied as a post-fold pass
    /// (`close_open_rows`) so it catches EVERY open row regardless of event
    /// ordering — including rows opened by stray post-terminal events that the
    /// engine emits after `NetCompleted` (e.g. a Map gather/collect that races
    /// net completion). Folding it inline at the terminal arm would miss those.
    close_with: Option<(StepStatus, DateTime<Utc>)>,
}

impl State {
    fn new() -> Self {
        Self::default()
    }

    fn absorb(&mut self, persisted: &PersistedEvent, lookups: &Lookups<'_>) {
        match &persisted.event {
            DomainEvent::TokenCreated {
                token, place_id, ..
            } => {
                self.note_entry_arrival(place_id, token, persisted.timestamp, lookups);
            }
            DomainEvent::TransitionFired {
                transition_id,
                consumed_tokens,
                produced_tokens,
                read_tokens,
                ..
            } => {
                self.handle_fire(
                    transition_id,
                    consumed_tokens,
                    produced_tokens,
                    read_tokens,
                    persisted.sequence,
                    persisted.timestamp,
                    lookups,
                    None,
                );
            }
            DomainEvent::EffectCompleted {
                transition_id,
                consumed_tokens,
                produced_tokens,
                read_tokens,
                ..
            } => {
                self.handle_fire(
                    transition_id,
                    consumed_tokens,
                    produced_tokens,
                    read_tokens,
                    persisted.sequence,
                    persisted.timestamp,
                    lookups,
                    None,
                );
            }
            DomainEvent::EffectFailed {
                transition_id,
                error_message,
                retryable,
                input_data,
                tokens_consumed,
                ..
            } => {
                let payload = serde_json::json!({
                    "error_message": error_message,
                    "retryable": retryable,
                    "tokens_consumed": tokens_consumed,
                    "input_data": input_data,
                });
                self.handle_failure(
                    transition_id,
                    payload,
                    persisted.sequence,
                    persisted.timestamp,
                    lookups,
                );
            }
            DomainEvent::NetCompleted { .. }
            | DomainEvent::NetCancelled { .. }
            | DomainEvent::NetFailed { .. } => {
                self.terminated = true;
                // Record the close intent; the actual terminalization is a
                // post-fold pass (`close_open_rows`) so it also catches rows
                // opened by any events that arrive AFTER the terminal one in
                // the buffer (out-of-order sequences / engine-emitted strays
                // post-completion). The FIRST terminal event wins — a net is
                // terminated exactly once; later lifecycle events (if any)
                // don't override the recorded outcome.
                let close_status = match &persisted.event {
                    DomainEvent::NetFailed { .. } => StepStatus::Failed,
                    _ => StepStatus::Skipped,
                };
                self.close_with
                    .get_or_insert((close_status, persisted.timestamp));
            }
            _ => {}
        }
    }

    /// Token landed at some place — if that place is the entry of a node,
    /// open a new iteration row for that node (dedup by TokenId).
    ///
    /// `arrived_at` is the event timestamp of the entry-token producer; it
    /// becomes the row's `started_at` so the displayed duration captures
    /// the full "input available → output written" span. For effect
    /// transitions (executor-backed steps) the engine only sees them when
    /// the result comes back, so using the firing's own timestamp would
    /// collapse duration to ~0; entry arrival is the only signal we have
    /// that the step's clock started.
    fn note_entry_arrival(
        &mut self,
        place_id: &PlaceId,
        token: &Token,
        arrived_at: DateTime<Utc>,
        lookups: &Lookups<'_>,
    ) {
        let Some(node_id) = lookups.entry_to_node.get(&place_id.0) else {
            return;
        };
        let seen = self.seen_entry_tokens.entry(node_id.clone()).or_default();
        if !seen.insert(token.id.clone()) {
            return; // Already opened a row for this entry token.
        }
        let iter = self.next_iter.entry(node_id.clone()).or_insert(0);
        let assigned = *iter;
        *iter += 1;
        self.active_iter.insert(node_id.clone(), assigned);
        self.rows
            .entry((node_id.clone(), assigned))
            .or_insert_with(|| StepExecutionRow {
                node_id: node_id.clone(),
                iteration_index: assigned,
                node_kind: lookups.kind_of(node_id),
                status: StepStatus::Pending,
                inputs: None,
                outputs: None,
                execution_id: None,
                branch_taken: None,
                started_at: Some(arrived_at),
                completed_at: None,
                error: None,
                last_sequence: 0,
            });
    }

    /// A produced token landed at some node's entry place: that token IS
    /// the consumer's inbound payload (the slim control token threaded by
    /// the upstream's `auto_output` arc, or the HumanTask-injected token
    /// from the `t_edge_*` wire transition). Record it on the consumer's
    /// row, attributed to the upstream node.
    ///
    /// Attribution prefers the firing transition's owner, but for the
    /// HumanTask wire-edge — credited to the consumer in
    /// `derive_node_ownership` so its `read_tokens` flow onto the right
    /// row — the firing owner IS the consumer, so we fall back to whichever
    /// place owner appears in `consumed_tokens` that isn't the consumer
    /// itself. That correctly resolves to the real upstream (e.g. Start).
    ///
    /// If a `read_tokens`-capture for the same producer slug arrives later
    /// in the same fire (or in a downstream owned fire), it wins on key
    /// collision — the parked envelope is semantically richer than the
    /// slim control.
    fn note_inbound_at_entry(
        &mut self,
        place_id: &PlaceId,
        token: &Token,
        transition_id: &TransitionId,
        consumed_tokens: &[(PlaceId, TokenId)],
        lookups: &Lookups<'_>,
    ) {
        let Some(consumer) = lookups.entry_to_node.get(&place_id.0).cloned() else {
            return;
        };
        let firing_owner = lookups.transition_owner.get(&transition_id.0).cloned();
        let source = match firing_owner {
            Some(o) if o != consumer => Some(o),
            _ => consumed_tokens
                .iter()
                .filter_map(|(p, _)| lookups.place_owner.get(&p.0))
                .find(|src| **src != consumer)
                .cloned(),
        };
        let key = source.unwrap_or_else(|| "input".to_string());

        let iter = self.active_iter.get(&consumer).copied().unwrap_or(0);
        // Row was opened by `note_entry_arrival` immediately above.
        let Some(row) = self.rows.get_mut(&(consumer, iter)) else {
            return;
        };
        let mut map: serde_json::Map<String, serde_json::Value> = row
            .inputs
            .as_ref()
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        // Only insert if absent — don't overwrite a read-arc payload
        // already on the row.
        if !map.contains_key(&key) {
            map.insert(key, token_color_to_json(&token.color));
            row.inputs = Some(serde_json::Value::Object(map));
        }
    }

    fn handle_fire(
        &mut self,
        transition_id: &TransitionId,
        consumed_tokens: &[(PlaceId, TokenId)],
        produced_tokens: &[(PlaceId, Token)],
        read_tokens: &[(PlaceId, Token)],
        sequence: u64,
        ts: DateTime<Utc>,
        lookups: &Lookups<'_>,
        forced_iter: Option<i32>,
    ) {
        // 1. Any token landing at some node's entry opens a new iteration row
        //    for that downstream node (handles upstream → downstream handoff,
        //    including Loop body re-entry). The firing's timestamp is the
        //    moment the entry token came into existence, which we record as
        //    the downstream step's `started_at`.
        //
        //    Simultaneously, that produced token is the consumer's inbound
        //    control token — what the upstream actually handed over the
        //    edge. Credit it to the consumer's row as an input, attributed
        //    to whichever upstream node owned the firing transition (or,
        //    when the firing transition is owned by the consumer itself —
        //    the HumanTask wire-edge case where `t_edge_*` is credited to
        //    the consumer so its `read_tokens` flow there — the source
        //    place owner from `consumed_tokens`). Without this the drawer's
        //    "Inputs" stays empty for steps that don't synthesize a
        //    `<slug>.<field>` read-arc, like a HumanTask whose form just
        //    collects user-entered data.
        for (place_id, token) in produced_tokens {
            self.note_entry_arrival(place_id, token, ts, lookups);
            self.note_inbound_at_entry(place_id, token, transition_id, consumed_tokens, lookups);
        }

        // 2. Find the owning node of this transition (if any) and update its
        //    active iteration row.
        let Some(owner) = lookups.transition_owner.get(&transition_id.0) else {
            return;
        };
        let owner = owner.clone();
        let iter = forced_iter
            .or_else(|| self.active_iter.get(&owner).copied())
            // Nodes whose only entry arrival is via control-flow (the rare case
            // where a node's first fire precedes any TokenCreated/
            // TransitionFired into its entry): open row at iteration 0
            // implicitly.
            .unwrap_or(0);

        if let std::collections::btree_map::Entry::Vacant(e) =
            self.rows.entry((owner.clone(), iter))
        {
            e.insert(StepExecutionRow {
                node_id: owner.clone(),
                iteration_index: iter,
                node_kind: lookups.kind_of(&owner),
                status: StepStatus::Pending,
                inputs: None,
                outputs: None,
                execution_id: None,
                branch_taken: None,
                started_at: None,
                completed_at: None,
                error: None,
                last_sequence: 0,
            });
            self.active_iter.insert(owner.clone(), iter);
            self.next_iter
                .entry(owner.clone())
                .and_modify(|n| *n = (*n).max(iter + 1))
                .or_insert(iter + 1);
        }

        let row = self
            .rows
            .get_mut(&(owner.clone(), iter))
            .expect("row just inserted above");

        // 3. First fire for this iteration: transition Pending→Running.
        // `started_at` was set when the entry token arrived (T0); only
        // backfill from the firing timestamp (T2) for any future control-flow
        // node whose first fire precedes any token deposit into its boundary.
        if row.status == StepStatus::Pending {
            row.status = StepStatus::Running;
            if row.started_at.is_none() {
                row.started_at = Some(ts);
            }
        }
        row.last_sequence = sequence;

        // 3b. Capture the executor `execution_id` the moment it appears. The
        // submit effect stamps it onto its OUTPUT token at DISPATCH — early,
        // while the step is still RUNNING — long before the business outputs are
        // parked at completion (5a, which hoists the same field). Without this a
        // running step has no `execution_id`, so the live datastream tap
        // (`/executions/{execution_id}/channels/{c}/data?follow=1`) can't address
        // a still-producing channel. Top-level only, mirroring the 5a hoist.
        if row.execution_id.is_none() {
            for (_place_id, token) in produced_tokens.iter().chain(read_tokens.iter()) {
                if let Some(eid) = token_color_to_json(&token.color)
                    .get("execution_id")
                    .and_then(|v| v.as_str())
                {
                    row.execution_id = Some(eid.to_string());
                    break;
                }
            }
        }

        // 4. Capture inputs from read_tokens, grouped by producer node.
        //    Merge into any existing inputs (set by the inbound-control-
        //    token capture in step 1, or by an earlier owned fire); the
        //    read-arc payload is the producer's parked envelope, which is
        //    semantically richer than the slim inbound control token, so
        //    it wins on key collision.
        if !read_tokens.is_empty() {
            let mut groups: BTreeMap<String, Vec<serde_json::Value>> = BTreeMap::new();
            for (place_id, token) in read_tokens {
                let key = lookups
                    .place_owner
                    .get(&place_id.0)
                    .cloned()
                    .unwrap_or_else(|| place_id.0.clone());
                groups
                    .entry(key)
                    .or_default()
                    .push(token_color_to_json(&token.color));
            }
            let mut inputs_obj: serde_json::Map<String, serde_json::Value> = row
                .inputs
                .as_ref()
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            for (k, mut vs) in groups {
                let v = if vs.len() == 1 {
                    vs.remove(0)
                } else {
                    serde_json::Value::Array(vs)
                };
                inputs_obj.insert(k, v);
            }
            row.inputs = Some(serde_json::Value::Object(inputs_obj));
        }

        // 5. Inspect produced_tokens for output-side terminations of this node.
        let iface = lookups.interface(&owner);
        let owner_kind = lookups.kind_of(&owner);
        for (place_id, token) in produced_tokens {
            // 5a. data_port deposit (Start/HumanTask/AutomatedStep/Loop/SubWorkflow park)
            if let Some(dp_owner) = lookups.data_port_to_node.get(&place_id.0) {
                if dp_owner == &owner {
                    let raw = token_color_to_json(&token.color);
                    // Hoist execution_id off the full envelope before unwrapping
                    // `outputs` to the business fields (the unwrap drops it).
                    if let Some(eid) = raw.get("execution_id").and_then(|v| v.as_str()) {
                        row.execution_id = Some(eid.to_string());
                    }
                    row.outputs = Some(canonical_output_payload(owner_kind, raw));
                    if row.status != StepStatus::Failed {
                        row.status = StepStatus::Completed;
                        row.completed_at = Some(ts);
                    }
                }
            }
            // 5b. Workflow-terminal deposit (End nodes)
            if let Some(t_owner) = lookups.workflow_terminal_to_node.get(&place_id.0) {
                if t_owner == &owner {
                    row.outputs = Some(canonical_output_payload(
                        owner_kind,
                        token_color_to_json(&token.color),
                    ));
                    if row.status != StepStatus::Failed {
                        row.status = StepStatus::Completed;
                        row.completed_at = Some(ts);
                    }
                }
            }
            // 5c. Branch output (Decision and friends with no data_port)
            if iface.map(|i| i.data_port.is_none()).unwrap_or(false) {
                if let Some((b_owner, key)) = lookups.output_to_node_branch.get(&place_id.0) {
                    if b_owner == &owner {
                        row.branch_taken = Some(key.to_string());
                        if row.status != StepStatus::Failed {
                            row.status = StepStatus::Completed;
                            row.completed_at = Some(ts);
                        }
                    }
                }
            }
            // 5d. Named "error" output on a parking-style node
            // (AutomatedStep / SubWorkflow). On the success path these
            // nodes finalize via 5a (data_port deposit); on the failure
            // path the retry-exhausted token routes out the named
            // "error" port while the data_port stays empty — without
            // this the row would be stuck at Running until net
            // termination. Capture the error token as `error` and flip
            // the row to Failed.
            if iface.map(|i| i.data_port.is_some()).unwrap_or(false) {
                if let Some((b_owner, key)) = lookups.output_to_node_branch.get(&place_id.0) {
                    if b_owner == &owner
                        && matches!(
                            key,
                            OutputKey::Edge(s) | OutputKey::Named(s) if s == "error"
                        )
                    {
                        row.status = StepStatus::Failed;
                        row.completed_at = Some(ts);
                        row.error = Some(token_color_to_json(&token.color));
                    }
                }
            }
        }
    }

    fn handle_failure(
        &mut self,
        transition_id: &TransitionId,
        error_payload: serde_json::Value,
        sequence: u64,
        ts: DateTime<Utc>,
        lookups: &Lookups<'_>,
    ) {
        let Some(owner) = lookups.transition_owner.get(&transition_id.0) else {
            return;
        };
        let owner = owner.clone();
        let iter = self.active_iter.get(&owner).copied().unwrap_or(0);
        let row = self
            .rows
            .entry((owner.clone(), iter))
            .or_insert_with(|| StepExecutionRow {
                node_id: owner.clone(),
                iteration_index: iter,
                node_kind: lookups.kind_of(&owner),
                status: StepStatus::Pending,
                inputs: None,
                outputs: None,
                execution_id: None,
                branch_taken: None,
                started_at: None,
                completed_at: None,
                error: None,
                last_sequence: 0,
            });
        if row.started_at.is_none() {
            row.started_at = Some(ts);
        }
        row.status = StepStatus::Failed;
        row.completed_at = Some(ts);
        row.error = Some(error_payload);
        row.last_sequence = sequence;
    }

    /// Post-fold pass: on terminal lifecycle, any row STILL `Pending`/
    /// `Running` after the whole event stream is folded is closed.
    /// Maps `NetFailed` → Failed; `NetCompleted`/`NetCancelled` → Skipped
    /// (the outcome recorded in `close_with` at the terminal event).
    ///
    /// A node still open at `NetCompleted` is in-flight work that a *different*
    /// branch superseded — e.g. a Timeout drains its body HumanTask when the
    /// timer wins, then routes to an End and the net completes; or a Map
    /// scatters K body iterations and the run abandons (or supersedes) some of
    /// them, leaving their per-(node, iteration) rows open. Such a row never
    /// produced its own output (its normal path marks it `Completed` at the
    /// data_port/terminal deposit), so it is NOT `Completed` — it was abandoned,
    /// same as under a whole-net cancel. Without this pass the row stayed stuck
    /// at `Running`/`Pending` forever even though the instance is `completed`
    /// (the editor badge never left the spinner).
    ///
    /// Running as a post-fold pass (rather than inline at the terminal arm) is
    /// what makes it robust to event ordering: rows opened by stray events that
    /// land AFTER the terminal event in the buffer (out-of-order sequence /
    /// engine-emitted post-completion strays, which Map's racing gather/collect
    /// can produce) are still caught. Started-but-not-finished rows get a
    /// `completed_at` so duration math works.
    fn close_open_rows(&mut self) {
        let Some((close_status, ts)) = self.close_with else {
            return;
        };
        for row in self.rows.values_mut() {
            if matches!(row.status, StepStatus::Pending | StepStatus::Running) {
                row.status = close_status;
                if row.completed_at.is_none() {
                    row.completed_at = Some(ts);
                }
            }
        }
    }

    /// After absorbing the terminal event, any registry node that never
    /// produced a row at all is recorded as `Skipped`.
    fn finalize_unreached(&mut self, registry: &InterfaceRegistry) {
        if !self.terminated {
            return;
        }
        for iface in registry.values() {
            if !self
                .rows
                .keys()
                .any(|(node_id, _)| node_id == &iface.node_id)
            {
                self.rows.insert(
                    (iface.node_id.clone(), 0),
                    StepExecutionRow {
                        node_id: iface.node_id.clone(),
                        iteration_index: 0,
                        node_kind: iface.kind,
                        status: StepStatus::Skipped,
                        inputs: None,
                        outputs: None,
                        execution_id: None,
                        branch_taken: None,
                        started_at: None,
                        completed_at: None,
                        error: None,
                        last_sequence: 0,
                    },
                );
            }
        }
    }

    fn into_rows(self) -> Vec<StepExecutionRow> {
        self.rows.into_values().collect()
    }
}

fn token_color_to_json(color: &TokenColor) -> serde_json::Value {
    match color {
        TokenColor::Unit => serde_json::Value::Null,
        TokenColor::Integer(n) => serde_json::Value::from(*n),
        TokenColor::Data(v) => v.clone(),
    }
}

/// Hoist the user-facing output payload out of a parked-envelope token so
/// `step_execution.outputs` shows the same shape downstream borrowers read
/// via `<slug>.<field>`. Mirrors `compile::producer_field_access_hoist`:
/// AutomatedStep + Agent envelopes are `{ detail: { outputs: {...}, ... }, ... }`,
/// HumanTask envelopes are `{ data: {...}, ... }`. Falls back to the raw
/// value if the expected nesting isn't present (legacy events / synthetic
/// tokens in unit tests stay shape-stable).
fn canonical_output_payload(kind: NodeKind, raw: serde_json::Value) -> serde_json::Value {
    let path: &[&str] = match kind {
        NodeKind::AutomatedStep | NodeKind::Agent => &["detail", "outputs"],
        NodeKind::HumanTask => &["data"],
        _ => return raw,
    };
    let mut cur = &raw;
    for seg in path {
        match cur.get(*seg) {
            Some(v) => cur = v,
            None => return raw,
        }
    }
    cur.clone()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::{NodeKind, OutputKey};
    use chrono::TimeZone;
    use petri_domain::{PersistedEvent, PlaceId, Token, TokenColor, TransitionId};
    use std::collections::BTreeMap;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0)
            .single()
            .expect("valid timestamp")
    }

    fn place(s: &str) -> PlaceId {
        PlaceId(s.to_string())
    }

    fn trans(s: &str) -> TransitionId {
        TransitionId(s.to_string())
    }

    fn data_token(payload: serde_json::Value) -> Token {
        Token::new(TokenColor::Data(payload))
    }

    fn unit_token() -> Token {
        Token::new_unit()
    }

    fn fired(
        seq: u64,
        ts_secs: i64,
        transition_id: TransitionId,
        produced: Vec<(PlaceId, Token)>,
        read: Vec<(PlaceId, Token)>,
    ) -> PersistedEvent {
        fired_with_consumed(seq, ts_secs, transition_id, vec![], produced, read)
    }

    fn fired_with_consumed(
        seq: u64,
        ts_secs: i64,
        transition_id: TransitionId,
        consumed: Vec<(PlaceId, TokenId)>,
        produced: Vec<(PlaceId, Token)>,
        read: Vec<(PlaceId, Token)>,
    ) -> PersistedEvent {
        PersistedEvent {
            sequence: seq,
            timestamp: ts(ts_secs),
            event: DomainEvent::TransitionFired {
                transition_id,
                transition_name: None,
                consumed_tokens: consumed,
                produced_tokens: produced,
                read_tokens: read,
                process_step_started: None,
                process_step_completed: None,
            },
            hash: String::new(),
            previous_hash: None,
        }
    }

    fn token_created(seq: u64, ts_secs: i64, place_id: PlaceId, token: Token) -> PersistedEvent {
        PersistedEvent {
            sequence: seq,
            timestamp: ts(ts_secs),
            event: DomainEvent::TokenCreated {
                token,
                place_id,
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            },
            hash: String::new(),
            previous_hash: None,
        }
    }

    fn net_completed(seq: u64, ts_secs: i64) -> PersistedEvent {
        PersistedEvent {
            sequence: seq,
            timestamp: ts(ts_secs),
            event: DomainEvent::NetCompleted {
                net_id: "test".to_string(),
                terminal_place_id: "p_end_result".to_string(),
                exit_code: None,
            },
            hash: String::new(),
            previous_hash: None,
        }
    }

    fn net_failed(seq: u64, ts_secs: i64, transition_id: TransitionId) -> PersistedEvent {
        PersistedEvent {
            sequence: seq,
            timestamp: ts(ts_secs),
            event: DomainEvent::NetFailed {
                net_id: "test".to_string(),
                transition_id,
                reason: "boom".to_string(),
                retryable: false,
            },
            hash: String::new(),
            previous_hash: None,
        }
    }

    fn effect_failed(
        seq: u64,
        ts_secs: i64,
        transition_id: TransitionId,
        msg: &str,
    ) -> PersistedEvent {
        PersistedEvent {
            sequence: seq,
            timestamp: ts(ts_secs),
            event: DomainEvent::EffectFailed {
                transition_id,
                transition_name: None,
                consumed_tokens: vec![],
                produced_tokens: vec![],
                effect_handler_id: "test_handler".to_string(),
                error_message: msg.to_string(),
                tokens_consumed: true,
                input_data: None,
                retryable: false,
            },
            hash: String::new(),
            previous_hash: None,
        }
    }

    /// Two-node graph: Start (`s`) → AutomatedStep (`a`) → End (`e`).
    /// `a` borrows `s.field`, parks its envelope at `p_a_data`, fires
    /// `t_a_park` to do so.
    fn three_node_registry() -> InterfaceRegistry {
        let mut reg: InterfaceRegistry = HashMap::new();

        let mut s = NodeInterface::new("s", NodeKind::Start);
        s.entry = Some("p_s_ready".to_string());
        s.outputs = BTreeMap::from([(OutputKey::Default, "p_s_main".to_string())]);
        s.data_port = Some("p_s_data".to_string());
        s.owned_places = vec![
            "p_s_ready".to_string(),
            "p_s_main".to_string(),
            "p_s_data".to_string(),
        ];
        s.owned_transitions = vec!["t_s_park".to_string()];
        reg.insert("s".to_string(), s);

        let mut a = NodeInterface::new("a", NodeKind::AutomatedStep);
        a.entry = Some("p_s_main".to_string()); // alias-collapsed onto producer
        a.outputs = BTreeMap::from([(OutputKey::Default, "p_a_main".to_string())]);
        a.data_port = Some("p_a_data".to_string());
        a.owned_places = vec!["p_a_main".to_string(), "p_a_data".to_string()];
        a.owned_transitions = vec!["t_a_park".to_string()];
        reg.insert("a".to_string(), a);

        let mut e = NodeInterface::new("e", NodeKind::End);
        e.entry = Some("p_a_main".to_string()); // alias-collapsed onto producer
        e.workflow_terminals = vec!["p_e_result".to_string()];
        e.owned_places = vec!["p_e_result".to_string()];
        e.owned_transitions = vec!["t_e_complete".to_string()];
        reg.insert("e".to_string(), e);

        reg
    }

    #[test]
    fn projector_attributes_start_park_to_node_s() {
        let reg = three_node_registry();
        let events = vec![
            token_created(0, 100, place("p_s_ready"), unit_token()),
            fired(
                1,
                101,
                trans("t_s_park"),
                vec![
                    (
                        place("p_s_data"),
                        data_token(serde_json::json!({"name": "Alice"})),
                    ),
                    (place("p_s_main"), unit_token()),
                ],
                vec![],
            ),
        ];

        let rows = project_step_executions(&events, &reg);

        let s_row = rows
            .iter()
            .find(|r| r.node_id == "s")
            .expect("s row exists");
        assert_eq!(s_row.status, StepStatus::Completed);
        assert_eq!(s_row.iteration_index, 0);
        assert_eq!(s_row.outputs, Some(serde_json::json!({"name": "Alice"})));
        // started_at = T0 (entry token created at 100), NOT T2 (firing at
        // 101). For effect-backed steps the firing event is emitted only
        // when the executor result returns; using the firing timestamp
        // would collapse duration to ~0.
        assert_eq!(s_row.started_at, Some(ts(100)));
        assert_eq!(s_row.completed_at, Some(ts(101)));
        assert_eq!(s_row.last_sequence, 1);
    }

    #[test]
    fn projector_captures_read_arc_inputs_grouped_by_producer() {
        let reg = three_node_registry();
        let events = vec![
            token_created(0, 100, place("p_s_ready"), unit_token()),
            fired(
                1,
                101,
                trans("t_s_park"),
                vec![
                    (
                        place("p_s_data"),
                        data_token(serde_json::json!({"name": "Alice"})),
                    ),
                    (place("p_s_main"), unit_token()),
                ],
                vec![],
            ),
            fired(
                2,
                102,
                trans("t_a_park"),
                vec![
                    (
                        place("p_a_data"),
                        data_token(serde_json::json!({"greeting": "hi Alice"})),
                    ),
                    (place("p_a_main"), unit_token()),
                ],
                vec![(
                    place("p_s_data"),
                    data_token(serde_json::json!({"name": "Alice"})),
                )],
            ),
        ];

        let rows = project_step_executions(&events, &reg);
        let a_row = rows
            .iter()
            .find(|r| r.node_id == "a")
            .expect("a row exists");
        assert_eq!(a_row.status, StepStatus::Completed);
        assert_eq!(
            a_row.inputs,
            Some(serde_json::json!({ "s": {"name": "Alice"} }))
        );
        assert_eq!(
            a_row.outputs,
            Some(serde_json::json!({"greeting": "hi Alice"}))
        );
    }

    /// Inbound control token capture: a step that doesn't synthesize any
    /// `<slug>.<field>` read-arcs still receives the upstream's slim
    /// control token at its entry. The projector records that token as
    /// an input from the upstream, attributed by the firing owner (the
    /// straightforward `upstream → downstream entry` path).
    #[test]
    fn projector_captures_inbound_control_token_when_upstream_produces_at_entry() {
        let reg = three_node_registry();
        // `a`'s entry is `p_s_main` (alias-collapsed onto the producer).
        // When `t_s_park` fires producing at `p_s_main`, `a`'s row should
        // open with that token recorded as an input from `s` — even
        // though there is no read-arc.
        let events = vec![
            token_created(0, 100, place("p_s_ready"), unit_token()),
            fired(
                1,
                101,
                trans("t_s_park"),
                vec![
                    (
                        place("p_s_data"),
                        data_token(serde_json::json!({"name": "Alice"})),
                    ),
                    (
                        place("p_s_main"),
                        data_token(serde_json::json!({"_instance_id": "abc"})),
                    ),
                ],
                vec![],
            ),
        ];

        let rows = project_step_executions(&events, &reg);
        let a_row = rows
            .iter()
            .find(|r| r.node_id == "a")
            .expect("a row exists (entry token landed even with no read-arc)");
        assert_eq!(
            a_row.inputs,
            Some(serde_json::json!({ "s": {"_instance_id": "abc"} })),
            "a should see the slim control token from s as its inbound input"
        );
    }

    /// Wire-edge inbound case (HumanTask in the real compiler): the
    /// transition that produces at the consumer's entry place is owned
    /// by the consumer itself (because `derive_node_ownership` credits
    /// `t_edge_*` transitions to the entry-receiving node so their
    /// `read_tokens` flow onto the right row). The firing owner is
    /// therefore == consumer, and inbound attribution must fall back
    /// to the source place owner from `consumed_tokens`.
    #[test]
    fn projector_credits_wire_edge_inbound_to_source_place_owner() {
        let mut reg: InterfaceRegistry = HashMap::new();

        let mut s = NodeInterface::new("s", NodeKind::Start);
        s.entry = Some("p_s_ready".to_string());
        s.outputs = BTreeMap::from([(OutputKey::Default, "p_s_main".to_string())]);
        s.data_port = Some("p_s_data".to_string());
        s.owned_places = vec![
            "p_s_ready".to_string(),
            "p_s_main".to_string(),
            "p_s_data".to_string(),
        ];
        s.owned_transitions = vec!["t_s_park".to_string()];
        reg.insert("s".to_string(), s);

        let mut h = NodeInterface::new("h", NodeKind::HumanTask);
        h.entry = Some("p_h_input".to_string());
        h.data_port = Some("p_h_data".to_string());
        h.outputs = BTreeMap::from([(OutputKey::Default, "p_h_main".to_string())]);
        h.owned_places = vec![
            "p_h_input".to_string(),
            "p_h_data".to_string(),
            "p_h_main".to_string(),
        ];
        // Wire-edge transition `t_edge_e1` belongs to the consumer (the
        // HumanTask) because `derive_node_ownership`'s post-pass attributes
        // wire-edge transitions to whichever node's entry they produce into.
        h.owned_transitions = vec!["t_h_finalize".to_string(), "t_edge_e1".to_string()];
        reg.insert("h".to_string(), h);

        // Upstream control token: we share the same id between the
        // produced-by-Start token and the consumed-by-wire-edge entry,
        // mirroring the real engine (no shared id is required for the
        // attribution rule, but matching reality keeps the test honest).
        let upstream_token = data_token(serde_json::json!({"_instance_id": "abc"}));
        let upstream_token_id = upstream_token.id.clone();
        let events = vec![
            token_created(0, 100, place("p_s_ready"), unit_token()),
            // t_s_park: Start parks its data + threads slim control onward.
            fired(
                1,
                101,
                trans("t_s_park"),
                vec![
                    (
                        place("p_s_data"),
                        data_token(serde_json::json!({"vendor": "ACME"})),
                    ),
                    (place("p_s_main"), upstream_token.clone()),
                ],
                vec![],
            ),
            // t_edge_e1: the HumanTask wire transition consumes from
            // p_s_main (Start's output) and produces at p_h_input
            // (HumanTask's entry). Owner = "h" (per the compiler fix).
            fired_with_consumed(
                2,
                102,
                trans("t_edge_e1"),
                vec![(place("p_s_main"), upstream_token_id.clone())],
                vec![(
                    place("p_h_input"),
                    data_token(serde_json::json!({"_instance_id": "abc"})),
                )],
                vec![],
            ),
        ];

        let rows = project_step_executions(&events, &reg);
        let h_row = rows
            .iter()
            .find(|r| r.node_id == "h")
            .expect("h row exists");
        assert_eq!(
            h_row.inputs,
            Some(serde_json::json!({ "s": {"_instance_id": "abc"} })),
            "wire-edge produced-at-entry token must be attributed to the source place owner (`s`), not the firing owner (`h` itself)"
        );
    }

    #[test]
    fn projector_loop_body_iterations_get_distinct_rows() {
        // Single body node `b` whose entry place receives 3 tokens (the Loop
        // body re-enters 3 times). Each entry arrival opens a new iteration
        // row. We don't model the Loop node itself here — just the body —
        // since that's the assertion the rest of the projector needs to
        // satisfy.
        let mut reg: InterfaceRegistry = HashMap::new();
        let mut b = NodeInterface::new("b", NodeKind::AutomatedStep);
        b.entry = Some("p_b_entry".to_string());
        b.data_port = Some("p_b_data".to_string());
        b.outputs = BTreeMap::from([(OutputKey::Default, "p_b_out".to_string())]);
        b.owned_places = vec![
            "p_b_entry".to_string(),
            "p_b_data".to_string(),
            "p_b_out".to_string(),
        ];
        b.owned_transitions = vec!["t_b_park".to_string()];
        reg.insert("b".to_string(), b);

        let mut events = Vec::new();
        let mut seq = 0u64;
        for iter in 0..3 {
            let entry_tok = Token::new_unit();
            events.push(token_created(
                seq,
                100 + iter * 10,
                place("p_b_entry"),
                entry_tok,
            ));
            seq += 1;
            events.push(fired(
                seq,
                101 + iter * 10,
                trans("t_b_park"),
                vec![
                    (
                        place("p_b_data"),
                        data_token(serde_json::json!({"i": iter})),
                    ),
                    (place("p_b_out"), unit_token()),
                ],
                vec![],
            ));
            seq += 1;
        }

        let rows = project_step_executions(&events, &reg);
        let b_rows: Vec<&StepExecutionRow> = rows.iter().filter(|r| r.node_id == "b").collect();
        assert_eq!(b_rows.len(), 3, "one row per iteration");
        for (idx, row) in b_rows.iter().enumerate() {
            assert_eq!(row.iteration_index, idx as i32);
            assert_eq!(row.status, StepStatus::Completed);
            assert_eq!(row.outputs, Some(serde_json::json!({"i": idx as i64})));
        }
    }

    #[test]
    fn projector_decision_records_branch_taken_as_edge_key() {
        // Decision node `d` has two output edges; the projector reports
        // `OutputKey::Edge("e_yes")` rendered as "edge:e_yes" when that
        // branch's place receives the token.
        let mut reg: InterfaceRegistry = HashMap::new();
        let mut d = NodeInterface::new("d", NodeKind::Decision);
        d.entry = Some("p_d_in".to_string());
        d.outputs = BTreeMap::from([
            (OutputKey::Edge("e_yes".to_string()), "p_d_yes".to_string()),
            (OutputKey::Edge("e_no".to_string()), "p_d_no".to_string()),
        ]);
        d.data_port = None;
        d.owned_places = vec![
            "p_d_in".to_string(),
            "p_d_yes".to_string(),
            "p_d_no".to_string(),
        ];
        d.owned_transitions = vec!["t_d_branch".to_string()];
        reg.insert("d".to_string(), d);

        let events = vec![
            token_created(0, 100, place("p_d_in"), unit_token()),
            fired(
                1,
                101,
                trans("t_d_branch"),
                vec![(place("p_d_yes"), unit_token())],
                vec![],
            ),
        ];

        let rows = project_step_executions(&events, &reg);
        let d_row = rows
            .iter()
            .find(|r| r.node_id == "d")
            .expect("d row exists");
        assert_eq!(d_row.status, StepStatus::Completed);
        assert_eq!(d_row.branch_taken.as_deref(), Some("edge:e_yes"));
        assert_eq!(
            d_row.outputs, None,
            "Decision nodes carry no data_port output"
        );
    }

    /// AutomatedStep retry-exhaustion routes a token out the node's named
    /// "error" output, *not* its data_port. The projector must treat that
    /// arrival as the step's terminal failure event — otherwise the
    /// success-side `data_port` deposit never fires and the row sticks at
    /// `Running` until net termination (which on this workflow doesn't
    /// happen, because the Failure node + downstream End complete the net
    /// normally with `result.ok = false`).
    #[test]
    fn projector_error_port_deposit_marks_parking_node_failed() {
        let mut reg = three_node_registry();
        // Re-register `a` with both Default + an "error" named output (the
        // same shape `lower_automated_step` publishes), and add the
        // exhaustion transition to its owned set so the fire is
        // attributed back to `a`.
        let a = reg.get_mut("a").expect("a registered");
        a.outputs = BTreeMap::from([
            (OutputKey::Default, "p_a_main".to_string()),
            (
                OutputKey::Edge("error".to_string()),
                "p_a_error".to_string(),
            ),
        ]);
        a.owned_places = vec![
            "p_a_main".to_string(),
            "p_a_data".to_string(),
            "p_a_error".to_string(),
        ];
        a.owned_transitions = vec!["t_a_park".to_string(), "t_a_exhausted".to_string()];

        let error_token = data_token(serde_json::json!({
            "job_id": "a", "run": 1, "retries": 0, "max_retries": 0, "reason": "failed"
        }));
        let events = vec![
            token_created(0, 100, place("p_s_ready"), unit_token()),
            fired(
                1,
                101,
                trans("t_s_park"),
                vec![
                    (
                        place("p_s_data"),
                        data_token(serde_json::json!({"name": "Alice"})),
                    ),
                    (place("p_s_main"), unit_token()),
                ],
                vec![],
            ),
            // a's exhausted transition deposits the failure token on
            // p_a_error; p_a_data (data_port) stays empty.
            fired(
                2,
                102,
                trans("t_a_exhausted"),
                vec![(place("p_a_error"), error_token.clone())],
                vec![],
            ),
        ];

        let rows = project_step_executions(&events, &reg);
        let a_row = rows
            .iter()
            .find(|r| r.node_id == "a")
            .expect("a row exists");
        assert_eq!(a_row.status, StepStatus::Failed);
        assert_eq!(a_row.completed_at, Some(ts(102)));
        assert_eq!(
            a_row.error,
            Some(serde_json::json!({
                "job_id": "a", "run": 1, "retries": 0, "max_retries": 0, "reason": "failed"
            }))
        );
        assert_eq!(
            a_row.outputs, None,
            "data_port stays empty on the failure path"
        );
    }

    #[test]
    fn projector_effect_failed_marks_row_failed_with_error_payload() {
        let reg = three_node_registry();
        let events = vec![
            token_created(0, 100, place("p_s_ready"), unit_token()),
            fired(
                1,
                101,
                trans("t_s_park"),
                vec![
                    (
                        place("p_s_data"),
                        data_token(serde_json::json!({"name": "Alice"})),
                    ),
                    (place("p_s_main"), unit_token()),
                ],
                vec![],
            ),
            effect_failed(2, 102, trans("t_a_park"), "io error"),
        ];

        let rows = project_step_executions(&events, &reg);
        let a_row = rows
            .iter()
            .find(|r| r.node_id == "a")
            .expect("a row exists");
        assert_eq!(a_row.status, StepStatus::Failed);
        assert_eq!(a_row.completed_at, Some(ts(102)));
        let err = a_row.error.as_ref().expect("error payload");
        assert_eq!(
            err.get("error_message"),
            Some(&serde_json::json!("io error"))
        );
    }

    #[test]
    fn projector_net_failed_marks_open_rows_failed_unreached_skipped() {
        let reg = three_node_registry();
        let events = vec![
            token_created(0, 100, place("p_s_ready"), unit_token()),
            fired(
                1,
                101,
                trans("t_s_park"),
                vec![
                    (
                        place("p_s_data"),
                        data_token(serde_json::json!({"name": "Alice"})),
                    ),
                    (place("p_s_main"), unit_token()),
                ],
                vec![],
            ),
            net_failed(2, 102, trans("t_a_park")),
        ];

        let rows = project_step_executions(&events, &reg);
        let a_row = rows.iter().find(|r| r.node_id == "a").expect("a row");
        let e_row = rows.iter().find(|r| r.node_id == "e").expect("e row");
        // `a` was entered (token at p_s_main = a.entry) but never fired
        // → still Pending, closed as Failed by NetFailed.
        assert_eq!(a_row.status, StepStatus::Failed);
        // `e` was never reached → Skipped (no entry token, no fire).
        assert_eq!(e_row.status, StepStatus::Skipped);
    }

    /// Regression: a node left `Running` when the net completes via another
    /// branch (e.g. a Timeout drains its body HumanTask, then the `timeout`
    /// branch routes to an End → `NetCompleted`) must be closed as `Skipped`,
    /// not stuck at `Running` and not falsely `Completed`. Before the fix
    /// `close_open_rows` returned early on `NetCompleted`, so the editor's
    /// node badge spun forever.
    #[test]
    fn projector_net_completed_closes_open_running_rows_skipped() {
        let reg = three_node_registry();
        let events = vec![
            token_created(0, 100, place("p_s_ready"), unit_token()),
            fired(
                1,
                101,
                trans("t_s_park"),
                vec![
                    (
                        place("p_s_data"),
                        data_token(serde_json::json!({"name": "Alice"})),
                    ),
                    // a.entry — opens a's row (Pending).
                    (place("p_s_main"), unit_token()),
                ],
                vec![],
            ),
            // `a` fires an owned transition but never deposits to its data_port
            // → Pending→Running, never completes (mimics a HumanTask awaiting a
            // signal that a wrapping Timeout later drains).
            fired(2, 102, trans("t_a_park"), vec![], vec![]),
            // The net completes via the other (timeout) branch.
            net_completed(3, 103),
        ];

        let rows = project_step_executions(&events, &reg);
        let a_row = rows.iter().find(|r| r.node_id == "a").expect("a row");
        assert_eq!(
            a_row.status,
            StepStatus::Skipped,
            "open Running row at NetCompleted must close as Skipped, not stay Running"
        );
        assert!(
            a_row.completed_at.is_some(),
            "closed row gets completed_at so duration math works"
        );
    }

    /// Map-in-Loop shape: the Map node (`mp`) scatters K items into a body
    /// node (`body`), opening one `(body, iter)` row per item. The run
    /// abandons some iterations mid-flight (the engine wedged / a superseding
    /// branch terminated the net) — those body rows are left `Pending`/
    /// `Running`, and the Map node's own row may also be open if its gather
    /// never produced. A `NetCompleted` then arrives. After the fold, EVERY
    /// row must be terminal (no `Pending`/`Running`) — the COMPLETED instance
    /// must not surface in-flight step rows.
    #[test]
    fn projector_net_completed_closes_open_map_iteration_rows() {
        let mut reg: InterfaceRegistry = HashMap::new();

        // Map node `mp`: entry p_mp_input, parks gathered output at p_mp_data.
        let mut mp = NodeInterface::new("mp", NodeKind::Map);
        mp.entry = Some("p_mp_input".to_string());
        mp.data_port = Some("p_mp_data".to_string());
        mp.outputs = BTreeMap::from([(OutputKey::Default, "p_mp_output".to_string())]);
        mp.owned_places = vec![
            "p_mp_input".to_string(),
            "p_mp_items".to_string(),
            "p_mp_results".to_string(),
            "p_mp_gathered".to_string(),
            "p_mp_data".to_string(),
            "p_mp_output".to_string(),
        ];
        mp.owned_transitions = vec![
            "t_mp_scatter".to_string(),
            "t_mp_dispatch".to_string(),
            "t_mp_gather".to_string(),
            "t_mp_yield".to_string(),
        ];
        reg.insert("mp".to_string(), mp);

        // Body node `body`: per-item entry p_body_in, parks at p_body_data.
        let mut body = NodeInterface::new("body", NodeKind::AutomatedStep);
        body.entry = Some("p_body_in".to_string());
        body.data_port = Some("p_body_data".to_string());
        body.outputs = BTreeMap::from([(OutputKey::Default, "p_body_out".to_string())]);
        body.owned_places = vec![
            "p_body_in".to_string(),
            "p_body_data".to_string(),
            "p_body_out".to_string(),
        ];
        body.owned_transitions = vec!["t_body_park".to_string()];
        reg.insert("body".to_string(), body);

        const K: usize = 3;
        let mut events = Vec::new();

        // 0: the workflow token lands at the Map node's entry.
        events.push(token_created(0, 100, place("p_mp_input"), unit_token()));

        // 1: t_mp_scatter — a SINGLE batch fire produces K item tokens, all at
        //    the body's entry place p_body_in. Each opens a (body, iter) row.
        let mut scatter_produced = Vec::new();
        for i in 0..K {
            scatter_produced.push((
                place("p_body_in"),
                data_token(serde_json::json!({ "x": i, "__map_idx": i })),
            ));
        }
        events.push(fired(
            1,
            101,
            trans("t_mp_scatter"),
            scatter_produced,
            vec![],
        ));

        // 2: only the FIRST body iteration actually completes (parks output).
        events.push(fired(
            2,
            102,
            trans("t_body_park"),
            vec![
                (
                    place("p_body_data"),
                    data_token(serde_json::json!({"y": 0})),
                ),
                (place("p_body_out"), unit_token()),
            ],
            vec![],
        ));

        // 3: NetCompleted arrives WITHOUT the remaining body iterations or the
        //    Map node's own gather/yield completing — the run abandoned them.
        events.push(net_completed(3, 200));

        let rows = project_step_executions(&events, &reg);

        // Every row must be terminal — nothing stuck pending/running.
        for r in &rows {
            assert!(
                !matches!(r.status, StepStatus::Pending | StepStatus::Running),
                "row ({}, {}) left non-terminal: {:?}",
                r.node_id,
                r.iteration_index,
                r.status
            );
            assert!(
                r.completed_at.is_some(),
                "terminal row ({}, {}) must have completed_at for duration math",
                r.node_id,
                r.iteration_index
            );
        }

        // Sanity: K body iteration rows exist; the first is Completed, the
        // abandoned ones are Skipped; the Map node's own row is Skipped.
        let body_rows: Vec<&StepExecutionRow> =
            rows.iter().filter(|r| r.node_id == "body").collect();
        assert_eq!(body_rows.len(), K, "one row per scattered item");
        let completed = body_rows
            .iter()
            .filter(|r| r.status == StepStatus::Completed)
            .count();
        assert_eq!(
            completed, 1,
            "exactly the one finished iteration is Completed"
        );
        let skipped = body_rows
            .iter()
            .filter(|r| r.status == StepStatus::Skipped)
            .count();
        assert_eq!(skipped, K - 1, "abandoned iterations close as Skipped");

        let mp_row = rows.iter().find(|r| r.node_id == "mp").expect("mp row");
        assert_eq!(
            mp_row.status,
            StepStatus::Skipped,
            "Map node whose gather never produced closes as Skipped, not Running"
        );
    }

    /// Ordering robustness: the engine can emit a stray body event AFTER
    /// `NetCompleted` in the buffer (Map's gather/collect racing net
    /// completion → an out-of-order or post-terminal `TransitionFired` that
    /// opens a fresh body iteration row). The terminal close must still catch
    /// it. The old inline close (run at the terminal arm, before the stray
    /// event was folded) left that row stuck `Pending`/`Running`; the post-fold
    /// pass closes it.
    #[test]
    fn projector_closes_rows_opened_after_terminal_event() {
        let mut reg: InterfaceRegistry = HashMap::new();
        let mut mp = NodeInterface::new("mp", NodeKind::Map);
        mp.entry = Some("p_mp_input".to_string());
        mp.data_port = Some("p_mp_data".to_string());
        mp.outputs = BTreeMap::from([(OutputKey::Default, "p_mp_output".to_string())]);
        mp.owned_places = vec!["p_mp_input".to_string(), "p_mp_data".to_string()];
        mp.owned_transitions = vec!["t_mp_scatter".to_string()];
        reg.insert("mp".to_string(), mp);

        let mut body = NodeInterface::new("body", NodeKind::AutomatedStep);
        body.entry = Some("p_body_in".to_string());
        body.data_port = Some("p_body_data".to_string());
        body.outputs = BTreeMap::from([(OutputKey::Default, "p_body_out".to_string())]);
        body.owned_places = vec![
            "p_body_in".to_string(),
            "p_body_data".to_string(),
            "p_body_out".to_string(),
        ];
        body.owned_transitions = vec!["t_body_park".to_string()];
        reg.insert("body".to_string(), body);

        let events = vec![
            token_created(0, 100, place("p_mp_input"), unit_token()),
            // scatter opens one body iteration row.
            fired(
                1,
                101,
                trans("t_mp_scatter"),
                vec![(place("p_body_in"), data_token(serde_json::json!({"x": 0})))],
                vec![],
            ),
            // NetCompleted arrives.
            net_completed(2, 200),
            // STRAY post-terminal event: opens a SECOND body iteration row
            // (Pending) AND fires its transition without parking output
            // (Running) — both must be terminalized by the post-fold pass.
            token_created(3, 201, place("p_body_in"), unit_token()),
            fired(4, 202, trans("t_body_park"), vec![], vec![]),
        ];

        let rows = project_step_executions(&events, &reg);
        for r in &rows {
            assert!(
                !matches!(r.status, StepStatus::Pending | StepStatus::Running),
                "row ({}, {}) opened after the terminal event left non-terminal: {:?}",
                r.node_id,
                r.iteration_index,
                r.status
            );
        }
        // Both body iterations exist and both are Skipped.
        let body_rows: Vec<&StepExecutionRow> =
            rows.iter().filter(|r| r.node_id == "body").collect();
        assert_eq!(
            body_rows.len(),
            2,
            "stray post-terminal arrival still opens a row"
        );
        assert!(
            body_rows.iter().all(|r| r.status == StepStatus::Skipped),
            "every body row closes Skipped under NetCompleted"
        );
    }

    #[test]
    fn projector_is_idempotent_under_replay() {
        let reg = three_node_registry();
        let events = vec![
            token_created(0, 100, place("p_s_ready"), unit_token()),
            fired(
                1,
                101,
                trans("t_s_park"),
                vec![
                    (
                        place("p_s_data"),
                        data_token(serde_json::json!({"name": "Alice"})),
                    ),
                    (place("p_s_main"), unit_token()),
                ],
                vec![],
            ),
            fired(
                2,
                102,
                trans("t_a_park"),
                vec![
                    (
                        place("p_a_data"),
                        data_token(serde_json::json!({"greeting": "hi"})),
                    ),
                    (place("p_a_main"), unit_token()),
                ],
                vec![],
            ),
            fired(
                3,
                103,
                trans("t_e_complete"),
                vec![(
                    place("p_e_result"),
                    data_token(serde_json::json!({"ok": true})),
                )],
                vec![],
            ),
            net_completed(4, 104),
        ];

        let first = project_step_executions(&events, &reg);
        let second = project_step_executions(&events, &reg);
        assert_eq!(first.len(), second.len());
        for (a, b) in first.iter().zip(second.iter()) {
            assert_eq!(a.node_id, b.node_id);
            assert_eq!(a.iteration_index, b.iteration_index);
            assert_eq!(a.status, b.status);
            assert_eq!(a.outputs, b.outputs);
            assert_eq!(a.last_sequence, b.last_sequence);
        }
    }

    /// AutomatedStep parks the executor's full terminal-status envelope at
    /// its `data_port`; the projector hoists `detail.outputs` so the
    /// `step_execution.outputs` column shows the same user-payload view
    /// that downstream `<slug>.<field>` borrows read (per
    /// `compile::producer_field_access_hoist`).
    #[test]
    fn projector_unwraps_automated_step_executor_envelope() {
        let reg = three_node_registry();
        let envelope = serde_json::json!({
            "detail": {
                "outputs": { "result": "swept", "answer": 42 },
                "outcome": { "type": "success" },
                "duration_ms": 432,
            },
            "execution_id": "exec-1",
            "job_id": "a",
            "status": "completed",
        });
        let events = vec![
            token_created(0, 100, place("p_s_ready"), unit_token()),
            fired(
                1,
                101,
                trans("t_s_park"),
                vec![
                    (
                        place("p_s_data"),
                        data_token(serde_json::json!({"name": "Alice"})),
                    ),
                    (place("p_s_main"), unit_token()),
                ],
                vec![],
            ),
            fired(
                2,
                102,
                trans("t_a_park"),
                vec![
                    (place("p_a_data"), data_token(envelope)),
                    (place("p_a_main"), unit_token()),
                ],
                vec![],
            ),
        ];

        let rows = project_step_executions(&events, &reg);
        let a_row = rows.iter().find(|r| r.node_id == "a").expect("a row");
        assert_eq!(a_row.node_kind, NodeKind::AutomatedStep);
        assert_eq!(
            a_row.outputs,
            Some(serde_json::json!({ "result": "swept", "answer": 42 })),
            "AutomatedStep outputs must be hoisted from detail.outputs"
        );
    }
}
