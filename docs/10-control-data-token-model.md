# Control / Data Token Model — Parked Data, Read-Arc Borrows, Producer-Namespaced Scope

Status: **Implemented (landed on `main`)**
Author: implementation + integration record (control/data foundation and follow-ups).
Landed by: `b804d82` (native foundation), `7743b6b` (borrow-reachable scope + trigger-ingestion type gate), `9108e14` (stale-test alignment), `8c277ea` (producer-namespaced slug refs), `e0695bb` (Start as parked producer + Process group + two-column ref picker).
Related code: `service/src/compiler/token_shape.rs`, `service/src/compiler/compile.rs` (`apply_control_data_foundation`), `service/src/compiler/lower.rs` (`split_outputs`, `park_outputs`), `service/src/compiler/error.rs` (`SlugConflict`), `service/src/models/template.rs` (`WorkflowNode.slug`), `service/src/handlers/templates.rs` (`/api/v1/analyze`), `app/src/lib/editor/guard-scope.ts`, `app/.../property-sections/RefPicker.svelte`.
Supersedes in part: [`05-typed-ports.md`](./05-typed-ports.md), [`07-runtime-port-enforcement.md`](./07-runtime-port-enforcement.md).

## 1. The problem this solved

Before the foundation, three independent representations of "what does the token look like at this node" coexisted and nothing forced them to agree:

1. The editor's design-time scope (`guard-scope.ts::computeScopes`) — a TS reimplementation that flattened *declared* upstream port fields into `input.<field>`.
2. The compiler's lowering — what actually happened to the token JSON (the `data` wrapper a human task adds, the executor envelope an automated step wraps everything in, `_`-prefixed metadata, the loop counter).
3. The runtime token — every business place was `DynamicToken` (= any); declared port schemas were enforced only at the system boundary, never inside the net.

Concrete failure (live invoice net): a `check-amount` decision guard `input.invoice_amount > 5000` was accepted by the flat model even though, at that point in the net, the token was the `extract` step's executor envelope and `invoice_amount` had only ever existed as a human-task form field nested under `.data`. The guard silently never matched → the default branch was taken → the run reported "completed" while having done the wrong thing. Plus: the fat accumulating token duplicated all upstream data on every event/hop.

## 2. The model

A node's **business output** is **parked**, write-once, in a `p_{id}_data` place. Only a slim **control token** — `_`-prefixed metadata, `task_id`, `status`, the loop counter — is threaded by-move through the net. A guard, loop condition, or End/Failure result-mapping that needs an upstream field gets a non-consuming **read-arc** (`ScenarioArc{read:true}`) into the parked place that owns it.

This maps onto Rust's ownership model — the mental model to use when reasoning about it:

| Concept | Rust | Net |
|---|---|---|
| A produced value | `let x = …` (owned, immutable) | `p_{id}_data` — write-once, **zero consuming arcs** (monotone invariant) |
| Reading it elsewhere | `&x` shared borrow | synthesized `read: true` arc |
| The thread of execution | `let mut` moved | `p_{id}_ctrl` — the only token that moves by-value |
| Use-after-drop | borrow-check error | hard `CompileError` (not a silently-missed branch) |

The compiler is the **borrow-checker**: provenance proves which parked place owns a referenced field and synthesizes the borrow. A reference nothing reachable owns is a compile error, surfaced pre-publish.

## 3. Lowering

`service/src/compiler/lower.rs`:

- **Data-yielders** (HumanTask, AutomatedStep) → `split_outputs`: the producer transition still emits `p_{id}_output`; a new `t_{id}_yield` transition (logic = `YIELD_LOGIC`) consumes it and emits `data → p_{id}_data` (the whole producer output, parked, write-once) and `ctrl → p_{id}_ctrl` (only `_*` / `task_id` / `status`). Downstream wiring consumes `p_{id}_ctrl`.
- **Start** → `park_outputs`: an *additive* fork, **not** a split. It forks `p_{id}_data` (for downstream read-arc borrows, so `start.<field>` is borrow-reachable exactly like `review.<field>`) **plus** `p_{id}_main` carrying the full token onward — so the immediately-following task can still interpolate Start fields off the control token (`{{ invoice_id }}`).
- Pass-through patterns (Decision, Split, Join, Loop, Phase, …) are unchanged.

`YIELD_LOGIC` (verbatim): `let d = tok; let c = #{}; for k in d.keys() { if k.starts_with("_") || k == "task_id" || k == "status" { c[k] = d[k]; } } #{ data: d, ctrl: c }`.

## 4. Read-arc synthesis (the borrow-checker)

`service/src/compiler/compile.rs::apply_control_data_foundation`, a pipeline phase that runs **after** `apply_merges` (so place ids are final):

1. Registers typed `#/definitions/*` for every split node: `Data__{id}` (the producer's structural shape), `Ctrl__{id}` (open object), and a permissive `DynamicToken` catch-all. Schemas the split places and yield-transition ports.
2. For every Decision/Loop guard and End/Failure result-mapping reference, resolves it via the shared resolver (§5) to the owning parked place, **adds a `read:true` arc + input port**, and **rebinds** the reference in the transition's Rhai (`<slug>.field` → the read-arc port var).
3. Safety net: any pre-existing `$ref` not in `definitions` gets a permissive `{}` so the runtime `SchemaRegistry` resolves every ref (an unresolvable ref *fails* validation).

No engine changes were needed — read-arcs, parked tokens (which don't block `NetCompleted`), and `definitions`-driven `SchemaRegistry` validation already existed.

## 5. References, scope, and the single resolver

**Producer-namespaced references.** Borrowed data is addressed `<slug>.<field>` (sub-paths allowed: `<slug>.detail.outputs`). `slug` is the producer node's **user-defined**, Rhai-identifier-safe key on `WorkflowNode.slug`:

- `slug_index()` builds the slug→node map. Two nodes with the same explicit slug → `CompileError::SlugConflict` (named, pre-publish). Nodes without an explicit slug derive a deterministic default (sanitized from id/label, collision-suffixed `_2`, `_3`, …) so existing/published templates keep working.
- `input.<path>` is reserved for genuinely **control-token-resident** leaves: Start fields still on the control token before any task, `_loop_*`, `task_id`, `status`. These identity/routing leaves are attributed to a synthetic **"Process"** group, not to whichever node last forwarded the token.
- **Clean-cut:** there is no legacy unqualified-`input` nearest-wins fallback. A non-control `input.<field>` is unbindable by construction — borrowed data must be qualified.
- **Loop bodies — control-token reads are first-iteration-only.** A node *inside a loop body* that reads an upstream/Start business field off the control token (`input.<field>` / Python `input.<field>` / `token.<field>`) sees it **only on iteration 0**: the loop's `t_continue` rebuilds the token each pass (`#{ body: <body_out>, data: … }`) and an envelope-stripping body (any AutomatedStep) drops it, so the read returns `undefined` / `AttributeError` on iteration 1+. The compiler rejects this at publish with `CompileError::LoopBodyStaleControlRef` (`validate_loop_body_control_refs`), pointing at the safe **parked-borrow** form `<producer_slug>.<field>` — a non-consuming read-arc into the producer's write-once `p_<id>_data` that survives every iteration (this is how `lp.iteration`, `bo.observations`, `start.<field>` work inside loops). The loop's own `<slug>` namespace and the genuine control leaves (`_*`/`task_id`/`status`) are exempt. Note the loop accumulator `init`/`merge_expr` live on the Loop node itself (evaluated at enter), so a seed like `observations ← input.observations` is fine there.

**One resolver, three consumers.** `guard_refs()` (raw `scan_dotted_refs` scanner + `rhai_scope::extract_qualified_refs` gating to exclude Rhai locals/keywords/strings/comments) → `resolve_ref()`. This single resolver feeds:

- `reachable_scope()` — the editor variable picker (what you *can* reference here),
- `check_guard()` — the diagnostics,
- `guard_readarc_plan()` — the actual read-arc synthesis.

Because all three share it, the picker offers exactly what the compiler binds, and no diagnostic contradicts the synthesis. Scope is attributed **by provenance, not nearest-wins**: distinct producers of the same key are distinct paths (`review.amount` vs `compliance.amount`), and a nearer non-parked node can never mask a farther parked one. (This replaced an earlier nearest-wins collapse that could silently drop a reachable field; the old `ScopeCollision` diagnostic was deleted as the ambiguity no longer exists.)

## 6. Runtime schema enforcement

`analyze()` derives a structural `TokenShape` per node; `to_json_schema()` lowers it to real JSON Schema in the AIR `definitions`:

- `Data__{id}` = the producer's shape — scalars strictly typed (`number`/`boolean`/`string`), objects `additionalProperties: true` (extra/optional keys allowed), `FileRef`/`Json`/`Any`/`Opaque` deliberately permissive `{}` (the "declared→enforced ramp" — tighten over time).
- `Ctrl__{id}` = open object; `DynamicToken` = permissive catch-all for everything non-split.

The engine `SchemaRegistry` validates every token crossing a schemed place/port. This is the layer that, e.g., correctly rejected a malformed `invoice_file` (a FileRef object declared by Start, sent as the scalar string `"example"` by a faulty trigger) at the `t_review_yield` boundary instead of letting garbage flow into OCR. Task #19 added the symmetric **ingestion** gate (`validate_token_against_port`) so the same contract is enforced *before* a net is created, not only mid-net.

## 7. Editor surface

`surface_types()` → `POST /api/v1/analyze` → `TypeSurface { place_schemas, scopes, diagnostics, graph_ok }`. It is pure and independent of `compile_to_air` succeeding, so a draft with an unstaged Python step (unpublishable) still gets full type surfacing while editing — feedback lands before publish, not at publish.

The editor (`guard-scope.ts::fetchNodeScopes`, debounced in `NodePropertyPanel.svelte`) consumes this — no client-side scope reimplementation. `RefPicker.svelte` is a two-column node→variable popover (producer column + that producer's variables, filterable) used by `GuardEditor` in simple and advanced modes.

## 8. Relationship to prior design docs

- [`05-typed-ports.md`](./05-typed-ports.md) (Proposal) framed the problem of the "magical `input` object" guards with no data contract. That contract now exists and is enforced; guard authoring is producer-namespaced `<slug>.field` resolved by the compiler-as-borrow-checker. Read 05 for problem framing; **this doc for the implemented model.**
- [`07-runtime-port-enforcement.md`](./07-runtime-port-enforcement.md) (Proposal) described the "running net is untyped, every place is `DynamicToken`" hole. That hole is now closed for split producers via `Data__*`/`Ctrl__*` schemas and the `SchemaRegistry`. The remaining permissive cases are the documented declared→enforced ramp (§6).

## 9. Known gaps / follow-ups

- **Strict ramp:** `FileRef`-as-scalar / `Json` / `Any` / `Opaque` and undeclared executor outputs are still permissive `{}`. Tightening is incremental and intentional.
- **Per-iteration loop-produced data keying** (re-borrow each iteration) is out of scope; current single write-once slot is sound for read-before-loop usage.
- **Task tracker:** #18 (human-task FieldKind coercion, `dea5bfd`), #19 (trigger-ingestion gate, in `7743b6b`), #20 (borrow-reachable scope, in `7743b6b`), #21 (stale `compiler_tests` alignment, `9108e14`), #22 (producer-namespaced slugs, `8c277ea`) are all on `main`; #18/#19/#21 may still show "pending" in the tracker pending end-to-end re-verification.

## 10. Key entry points

| Concern | Symbol / file |
|---|---|
| Shape model | `TokenShape`, `ScalarTy`, `Provenance`, `analyze()` — `token_shape.rs` |
| Schema lowering | `to_json_schema()`, `data_def_name`/`ctrl_def_name`/`dynamic_token_definition` |
| The split / fork | `split_outputs`, `park_outputs`, `YIELD_LOGIC` — `lower.rs` |
| Read-arc synthesis | `apply_control_data_foundation` — `compile.rs` |
| Slugs | `WorkflowNode.slug` (`models/template.rs`), `slug_index()`, `CompileError::SlugConflict` |
| One resolver | `guard_refs()` → `resolve_ref()`; consumers `reachable_scope()`, `check_guard()`, `guard_readarc_plan()` |
| Editor surface | `surface_types()` → `/api/v1/analyze`; `guard-scope.ts`, `RefPicker.svelte` |
| Ingestion gate | `validate_token_against_port()` (`token_shape.rs`), trigger dispatcher |
| Tests | `service/tests/token_shape_prototype.rs`, inline `scope_reachability_tests`, `compiler_tests.rs`, `compiler_e2e.rs` |
