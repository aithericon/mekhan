# Multiple Start Nodes — Design Handoff

Status: **not implemented / blocked at the compiler only**. This is a scoped
handoff for a future session. No behavioural change has been made for this;
the document only captures the current state and what enabling it entails.

## Summary

A template currently must have **exactly one** `Start` node. Publishing a
template with 0 or 2+ Starts fails compilation. However, every layer
*downstream* of the compiler (wire types, instance-creation API, create-instance
UI) is already written to handle multiple Starts. The single hard blocker is the
compiler's start-count guard plus its single-root graph analysis.

This came up while building the Trigger → Start "entrypoint" UX: multiple Starts
+ multiple triggers is a coherent model (e.g. a webhook trigger seeds Start A, a
cron trigger seeds Start B, in the same template), so it is worth enabling
deliberately rather than by accident.

## Current state

### What blocks it (compiler — `service/src/compiler/compile.rs`)

- `validate()` — hard count check:
  ```rust
  let start_count = graph.nodes.iter()
      .filter(|n| matches!(n.data, WorkflowNodeData::Start { .. })).count();
  if start_count != 1 {
      return Err(CompileError::Validation(format!(
          "expected exactly one Start node, found {start_count}")));
  }
  ```
  (`compile.rs:462`, approx.)
- `build()` — keeps a single `start: Option<NodeIndex>`, overwriting on each
  Start it sees, and errors on found-0:
  ```rust
  let start = start.ok_or_else(|| CompileError::Validation(
      "expected exactly one Start node, found 0".into()))?;
  ```
  (`compile.rs:286`, approx.) `wg.start` is then used as the **single BFS root**
  for the reachability check, and the cycle/unreachable analysis assumes that
  one root.

### What is already plural-Start-aware (no change needed)

- `service/src/models/instance.rs`
  - `StartToken { start_block_id: String, token: serde_json::Value }` — keyed by
    the Start node id (`instance.rs:75`).
  - `CreateInstanceRequest.start_tokens: Vec<StartToken>` — a vector, documented
    as "Typed seeds for **each** Start block in the template. A Start with a
    non-empty `initial` port requires a matching entry here." (`instance.rs:85`).
- The create-instance dialog (`app/src/lib/components/instances/CreateInstanceDialog.svelte`)
  already loads **all** Start nodes from a template and renders a form per Start.
- `service/src/models/template.rs` model comments consistently describe initial
  tokens as seeded **per-Start** at instance creation time.
- The Start node now has a left-side `target` handle (added for the trigger
  entrypoint work), so the editor can already draw multiple Starts and wire
  triggers to each independently.

## What enabling it would take

Concentrated in `service/src/compiler/compile.rs`; **not** a model/wire/UI
rewrite.

1. **Relax the count guard.** Replace `start_count != 1` with `start_count == 0`
   (still require ≥1 Start; 0 is genuinely invalid — nothing seeds the net).
2. **Multi-root graph analysis.** `WorkflowDiGraph::build` must keep
   `starts: Vec<NodeIndex>` instead of a single `start`. The reachability check
   (`Bfs::new`) needs to seed from every Start (BFS from each root, union the
   visited set), or introduce a virtual super-source node with edges to every
   Start and BFS from that. The "unreachable nodes" error must then mean
   "unreachable from *any* Start".
3. **Cycle / DAG check.** `is_cyclic_directed(&wg.dag)` is root-independent, so
   it should be fine, but re-verify once multi-root reachability lands (a node
   reachable only from Start B must not be reported unreachable just because
   Start A's BFS didn't touch it).
4. **Codegen / AIR emission.** Confirm each Start emits its own
   `p_{id}_ready` place (it already does — `expand_node` handles Start
   per-node and ignores inbound edges; see `compile.rs:1277`). Verify the
   parameterization step (`parameterize_air`) maps each `StartToken` by
   `start_block_id` to the right place (this is already per-Start by design —
   confirm with a 2-Start integration test).
5. **Validation/UX.** Decide and enforce semantics for partially-seeded
   instances: `CreateInstanceRequest` docs already say a Start with a non-empty
   `initial` port *requires* a matching `start_tokens` entry; multi-Start makes
   "which Starts must be seeded for this instance" a real question (all of them?
   any subset, with un-seeded Starts simply not firing?). This is the main
   open design decision — see below.

## Open design questions

- **Seeding semantics with N Starts:** must every Start be seeded at instance
  creation, or can an instance seed a subset (the others stay dormant / are
  fired later by a Signal-kind trigger)? This interacts with the trigger model:
  a Spawn trigger seeds exactly one Start; a multi-Start template fired by a
  single trigger would only seed that trigger's target Start.
- **Completion semantics:** with multiple Starts there can be multiple
  concurrent token sources — does instance completion mean "all End nodes
  reached" / "any" / per-branch? Confirm against the existing terminal-place
  fixup logic before changing the count guard.
- **Editor affordances:** `NODE_PALETTE` marks Start with `maxInstances: 1`
  (`app/src/lib/types/editor.ts`). That cap (wherever it is enforced in the
  palette/drop path) must be lifted in lockstep with the compiler change, or
  authors still won't be able to add a second Start.

## Risks

- Loosening the compiler guard without the multi-root BFS fix would let
  templates publish where a whole Start's subgraph is silently unreachable from
  the (arbitrarily-chosen) single root — a correctness regression. Items 1 and 2
  must land together.
- The "exactly one Start" assumption may be relied on implicitly elsewhere
  (projections, instance lifecycle, completion detection). Grep `WorkflowNodeData::Start`
  across `service/src` before changing semantics — there are usages in the
  causality projector / instance handlers that assume a single entry.

## Acceptance criteria (for the future session)

- A template with 2 Start nodes, each with its own `initial` port and its own
  trigger, compiles and publishes.
- Creating an instance with `start_tokens` for both Starts seeds both
  `p_{id}_ready` places; firing either trigger seeds only its target Start.
- Reachability/cycle validation is correct under multiple roots (a node
  reachable only from the second Start is **not** flagged unreachable).
- Existing single-Start templates are byte-for-byte unaffected (regression
  suite green).
