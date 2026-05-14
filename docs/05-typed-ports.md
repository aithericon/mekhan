# Typed Ports for the Workflow Editor

Status: Proposal
Author: handoff doc — captures the design conversation, ready for an owner to refine and implement.
Related: [`03-mvp-architecture.md`](./03-mvp-architecture.md), `service/src/models/template.rs`, `service/src/compiler/compile.rs`, `service/src/petri/instance.rs`

## 1. Problem

The editor's block-graph model (`WorkflowNodeData` in `service/src/models/template.rs:80`) has no first-class data contract. Edges between blocks carry nothing typed. Data shows up in three implicit, unvalidated places:

- `Start { initialData: Option<Value> }` — opaque JSON blob, no schema.
- `Decision { conditions: Vec<BranchCondition { guard: String }> }` — Rhai expressions referencing a magical `input` object (e.g. `"input.approved == true"`, see `service/src/compiler/compile.rs:1142`). Nothing checks that any upstream block actually emits `approved`.
- `AutomatedStep { executionSpec: { config: Value } }` — free-form JSON per backend, no declared inputs or outputs.

`HumanTask` is the one exception: `TaskFieldConfig` (`service/src/models/template.rs:268`) gives form fields a name and kind. Those names are already the de-facto outputs of a human task — they just aren't lifted into a graph-visible "this block produces these fields" view.

Instance parameterization compounds the gap. `service/src/petri/instance.rs:14` merges the API caller's `metadata` blob into **every initial token of every Start place**, with no targeting and no validation. As soon as a template has two Start places with different shapes, this breaks silently.

This blocks several downstream features that all want the same thing:

- **Triggers** (cron, webhook, catalog-spawn, completion-chain) need to wire event payloads into specific entry-point tokens with specific shapes.
- **Catalog subscriptions into running nets** (`service/src/catalogue/subscriptions.rs`) already inject signals into places but have no declared contract for the token shape — current code works by reading the AIR.
- **Publish-time validation**: today a Decision can reference a field that no upstream block produces and it'll only fail at runtime.
- **Editor UX**: no autocomplete for guard expressions, no field-aware payload mappers, no type checking on edge connection.

## 2. Goals & Non-Goals

**Goals**
- Every block declares typed **input ports** and **output ports** with named fields.
- Edges carry typed payloads from a specific output port to a specific input port.
- The compiler validates the graph at publish time: every referenced field resolves to a real upstream port; every edge type-checks.
- The editor renders ports as connection handles, offers field autocomplete in expression contexts, and surfaces type mismatches as you connect.
- Trigger nodes wire to a target Start block's input port using a payload-mapping UI driven by that port's declared field schema.

**Non-goals**
- Replacing Rhai as the guard language. Keep Rhai; just put a schema-aware UI in front of it and validate at publish.
- Structural subtyping or generics. Nominal types, exact-match field schemas, with `Json` and `Any` as escape hatches.
- Changing the AIR / Petri-net runtime. Ports are a pre-compile concept; the compiler still emits the same AIR shape (typed tokens land in colored places exactly as today).
- Per-arc colors in the editor. Ports are at the block boundary; what flows along Petri arcs internal to an expanded block is the compiler's business.

## 3. Design Overview

### 3.1 Field kinds

Reuse and extend `TaskFieldKind` (`service/src/models/template.rs:285`):

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Textarea,
    Number,
    Bool,        // new — currently piggybacks on Checkbox
    Select,      // value set in field options
    File,        // catalog reference: file_metadata::StoragePath
    Signature,
    Timestamp,   // new — needed for trigger fire times, audit fields
    Json,        // opaque; escape hatch for legacy / dynamic payloads
}
```

`TaskFieldKind` becomes an alias / re-export — same wire format — so existing human task forms keep working.

### 3.2 Ports

```rust
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PortField {
    pub name: String,           // identifier, snake_case
    pub label: String,          // display
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,   // for Select
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Port {
    pub id: String,              // unique within the block (e.g. "in", "out", "approved", "rejected")
    pub label: String,
    pub fields: Vec<PortField>,  // the token shape this port produces or consumes
}
```

A port is a named bundle of typed fields. Two ports type-match if their field sets are equal (same names, same kinds, `Any` matches anything). One token flows per port firing.

### 3.3 Block-level port declaration

Each `WorkflowNodeData` variant either declares ports explicitly (user-editable) or derives them from existing config. Sketch:

```rust
pub enum WorkflowNodeData {
    Start {
        label,
        description,
        // NEW: declared input schema; the token this Start emits has this shape.
        // Defaults to empty fields for back-compat.
        initial: Port,
        // initialData removed; initial-token construction moves to triggers / instance API.
    },

    End {
        label,
        description,
        // NEW: what the final token must look like to terminate cleanly.
        // Empty = accept anything (default for migration).
        terminal: Port,
    },

    HumanTask {
        label, description, taskTitle, instructionsMdsvex,
        steps: Vec<TaskStepConfig>,
        // Derived port: union of all `TaskFieldConfig.name` across all steps.
        // Computed by `WorkflowNodeData::output_ports()`; not stored.
    },

    AutomatedStep {
        label, description,
        executionSpec: ExecutionSpecConfig,
        // NEW: declared output schema for downstream blocks.
        // Backend-specific defaults provided (see §3.4).
        output: Port,
    },

    Decision {
        label, description,
        conditions: Vec<BranchCondition>,
        defaultBranch,
        // Input port = whatever flows in (carried by the incoming edge).
        // Output ports = one per branch, each carrying the same input scope through.
        // Computed; not stored.
    },

    ParallelSplit { /* fan-out: input carried unchanged to each downstream */ },
    ParallelJoin  { /* fan-in: declared merge port (see §3.5) */ },
    Loop          { /* same as decision: scope carries through, plus iteration counter */ },
    Scope         { /* sub-graph; declares ports as block boundary */ },
}
```

Every variant gains a uniform accessor:

```rust
impl WorkflowNodeData {
    pub fn input_ports(&self)  -> Vec<Port>;  // some derived, some declared
    pub fn output_ports(&self) -> Vec<Port>;
}
```

The editor and compiler both use this — no special-casing per block kind at the call site.

### 3.4 Backend-derived output for `AutomatedStep`

Each `ExecutionBackendType` (`service/src/models/template.rs:324`) gets a default output port. Users can override.

| Backend     | Default output port `out` fields                       |
|-------------|--------------------------------------------------------|
| `python`    | `result: Json`                                         |
| `process`   | `stdout: Textarea`, `stderr: Textarea`, `exit_code: Number` |
| `docker`    | same as `process` plus `image: Text`                   |
| `http`      | `status_code: Number`, `body: Json`, `headers: Json`   |
| `llm`       | `text: Textarea`, `usage: Json`                        |
| `file_ops`  | `files: Json` (array of catalog refs)                  |
| `kreuzberg` | `text: Textarea`, `metadata: Json`                     |

These shapes are the implicit contract today — codifying them turns them into something the editor and compiler can use.

### 3.5 Edges

Extend `WorkflowEdge` (`service/src/models/template.rs:359`) with a target handle:

```rust
pub struct WorkflowEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub source_handle: Option<String>,   // already present; will become required post-migration
    pub target_handle: Option<String>,   // NEW; required post-migration
    pub label: Option<String>,
    pub edge_type: String,
}
```

`source_handle` already exists for xyflow handles (used today for Decision branch outputs). `target_handle` is the new bit — without it, an edge from `AutomatedStep.out` to a `Decision` is ambiguous if the Decision ever grows a second input port.

### 3.6 Scope resolution for guards

A guard expression at any node sees a `scope` = all fields produced by all reachable upstream nodes through the graph (deduplicated by `(node_id, field_name)`). The editor surfaces this scope in the Rhai expression builder as autocomplete. The compiler validates that every identifier in every guard resolves against the scope.

Concretely: today's `"input.approved == true"` becomes either `"approval_step.approved == true"` (qualified by source block) or, with a `using` clause in the Decision config, `"approved == true"` after declaring which upstream port supplies the implicit scope. Pick one — qualified-by-default is less magic and avoids ambiguity when two upstream blocks both produce `approved`.

## 4. Compiler Changes

Today `service/src/compiler/compile.rs` walks blocks and edges and emits places + transitions + arcs. Changes:

1. **Pre-pass: schema resolution.** For each node, compute `input_ports()` / `output_ports()`. Build a `NodeId → Vec<Port>` map.
2. **Edge validation.** Every edge must reference a real `(source_handle, target_handle)` pair; the two ports' field sets must be type-compatible. Fail compile with a clear error pointing to the edge and the mismatch.
3. **Scope walk.** Topologically traverse the graph, accumulating a scope map: `NodeId → { qualified_field_name → FieldKind }`. Use this for guard validation. For nodes inside loops or scopes, scope inherits from the enclosing block boundary.
4. **Guard validation.** Parse each Rhai guard, walk identifiers, check each one against the scope at that node. Reject unresolved or mistyped references at publish time.
5. **AIR emission.** Place colors come from the *input* port of the consuming transition. Initial tokens for Start places are constructed from the Start's declared `initial` port (no more generic metadata merge in `parameterize_air`).

The compiler stays the source of truth for the AIR; ports never leak into the runtime.

## 5. Instance API Changes

Today: `POST /api/instances { template_id, metadata: Value }`.

After:

```jsonc
POST /api/instances
{
  "template_id": "...",
  "start_tokens": [
    { "start_block_id": "n_start_1", "token": { "customer_id": "c-42", "doc_uri": "s3://..." } }
  ],
  "metadata": { /* free-form audit/system metadata, not merged into tokens */ }
}
```

`service/src/petri/instance.rs::parameterize_air` is rewritten to:
- Take `start_tokens: Vec<(start_block_id, token_value)>`.
- Validate each token against the Start block's `initial` port (required fields present, kinds match).
- Seed only the named Start places.
- Inject system fields (`_instance_id`, `_template_id`, `_template_version`, `_created_at`, `_created_by`) into seeded tokens only.

The existing global `metadata`-into-every-initial-token behavior is removed — it was unsafe and is what this proposal is fixing.

## 6. Editor Changes

Frontend lives in `app/` (SvelteKit + xyflow + Yjs). Concrete deltas:

- **Port handles.** Each xyflow node renders a labeled handle per port. Source handles on the right edge (one per output port), target handles on the left edge. Already partially supported — Decision branch outputs use named source handles today.
- **Port editor panel.** Side panel section "Inputs / Outputs" on selected node. For Start blocks: full CRUD on the `initial` port's field list (reuse the existing human-task-form field editor — same `FieldKind` and component). For AutomatedStep: editable `output` port with backend-suggested defaults. For HumanTask / Decision / etc.: read-only, computed from existing config with a "(derived from steps)" hint.
- **Edge connection UX.** When dragging from an output handle, valid input handles on other nodes light up; incompatible ports dim out. On drop, if no `target_handle` was hit, prompt to pick one.
- **Expression builder.** Decision guards (and similar Rhai-bearing fields) get a field picker showing the in-scope variables at that node. Free-form Rhai input stays available for power users; the field picker just inserts qualified names.
- **Publish validation surfacing.** Server returns a structured list of compile errors with `{ node_id?, edge_id?, message, kind }`. Editor highlights the offending nodes/edges in red and shows the message inline.

## 7. Migration

Existing templates need to keep loading. Plan:

1. **Deserialization defaults.** New port fields default to empty / derived. `Start.initial` defaults to an empty-fields port; `End.terminal` to empty; `AutomatedStep.output` to the backend's default shape. Edges with missing `target_handle` default to the consuming node's single canonical input port; compile fails only if the node has multiple input ports.
2. **Decision guards.** Existing guards reference unqualified `input.X`. Migration script walks each template, finds Decision nodes, looks at the single upstream port reaching that Decision, rewrites `input.X` → `<upstream>.X`. Templates that can't be rewritten unambiguously get flagged for manual review.
3. **Instance API.** Keep the old `metadata`-blob form working for one release behind a deprecation header. The new `start_tokens` form is preferred; if absent and the template has a non-empty `initial` schema, the API returns `400 missing_start_tokens`.
4. **Stored `initialData`.** Drop on read; values are migrated into the new `start_tokens` form via a one-time backfill script that wraps the old blob as `{ "n_start_1": {<blob>} }` for each existing instance record (audit only — instances don't re-instantiate).

No Petri-net runtime changes, no AIR format changes — migration is purely in the editor model, compiler, and instance API.

## 8. Phased Delivery

Each phase is independently shippable. Cut a phase early if priorities shift; later features land cleanly on earlier ones.

**Phase 1 — Start ports + new instance API.** ~1 week.
- Add `Port`, `PortField`, `FieldKind` types.
- Add `initial: Port` to `Start`. Field-editor panel in the editor.
- Rewrite `parameterize_air` around `start_tokens`. New instance API form. Old form behind deprecation.
- *Unblocks*: trigger payload mapping has a real target.

**Phase 2 — Edge target handles + AutomatedStep output ports.** ~1 week.
- Add `target_handle` to `WorkflowEdge`. Editor wiring UX for it.
- Add `output: Port` to `AutomatedStep` with backend defaults.
- Compiler validates edge type compatibility.
- *Unblocks*: type-checked graphs at publish.

**Phase 3 — Scope walk + guard validation.** ~1.5 weeks.
- Topological scope resolver in the compiler.
- Rewrite guards to qualified field references; migration script.
- Field-picker UI in expression contexts.
- *Unblocks*: publish-time validation of all guards; autocomplete in editor.

**Phase 4 — Lift HumanTask outputs; fill in remaining block kinds.** ~1 week.
- `HumanTask` exposes derived output port from its `TaskFieldConfig` list.
- `Decision`, `ParallelSplit/Join`, `Loop`, `Scope` get their port derivation logic.
- Editor renders derived ports read-only.
- *Unblocks*: every block participates uniformly; the model is "done."

**Phase 5 — Trigger nodes** (separate proposal). Builds on Phases 1–4; trigger payload mappers consume the Start `initial` schema.

## 9. Open Questions

These are deferred decisions, not blockers — call them out when an owner picks this up.

1. **Qualification syntax in Rhai guards.** Qualified-by-default (`approval_step.approved`) vs. a `using upstream_node` clause that re-introduces unqualified `input.X`. Recommendation: qualified-by-default; less magic, no ambiguity.
2. **Multiple ports of the same name across blocks.** Allow it (qualified names make it unambiguous) or forbid for clarity. Recommendation: allow.
3. **Type system.** Stay nominal-by-field-name, or move to a tagged-union `TokenSchema` (JSON-Schema-lite)? Recommendation: stay nominal until someone needs nested objects; `Json` is the escape hatch.
4. **Scope across `Scope` blocks and `Loop`s.** Does a loop's body see the outer scope plus loop-locals (iteration index, accumulator)? Recommendation: yes — same model as lexical scope; loop-locals shadow outer fields by name.
5. **Catalog subscription signals.** Today `service/src/catalogue/subscriptions.rs` sends `ExternalSignal` to a place without a declared schema. Should the *target place* also have a port declaration? Recommendation: yes, in Phase 5 with triggers — signals into running nets get the same payload-mapping UI as triggers into new instances.
6. **AIR round-trip.** Should ports be persisted alongside the AIR (e.g. as a sidecar field in `air_json`) so the runtime could surface field names in event logs? Recommendation: no for now — keep ports compile-time-only. Revisit if observability needs it.

## 10. Out of Scope (future work)

- **Generic / parametric ports.** A `ParallelSplit` that fans the same payload to N branches is fine as N identical ports; real generics aren't needed yet.
- **Sub-template ports.** When `Scope` blocks become invokable sub-templates, they'll need a stable port interface — covered when sub-templates become first-class.
- **Live data preview in the editor.** Showing actual token values from running instances at each port (the dev-tools view) is downstream of having a port model at all.
- **Schema versioning.** Editing a published template's Start port shape and reconciling against in-flight instances. Templates already have a `version` chain (`base_template_id`, `parent_id`, `version`) — schema migrations across versions are a separate concern.

## 11. Acceptance Criteria

Phase 1 ships when:
- Existing templates load and publish unchanged.
- A new template with a Start `initial` port containing two required fields rejects `POST /api/instances` without `start_tokens` matching the schema.
- The editor's field-editor panel can add/remove/rename fields on a Start block, with the change round-tripping through publish and reload.
- `instance_lifecycle_e2e.rs` covers both forms of the instance API (legacy blob, new `start_tokens`) until the legacy form is removed.

Full proposal ships when:
- Every `WorkflowNodeData` variant returns non-empty `input_ports()` and `output_ports()` where semantically meaningful.
- Every edge in a publishable template references a valid `(source_handle, target_handle)` and type-checks.
- Every guard in a publishable template resolves cleanly against its node's scope.
- A trigger node (Phase 5) can be added to a template, wired to a Start port, and its payload mapper renders a per-field UI driven by that port's declared schema.
