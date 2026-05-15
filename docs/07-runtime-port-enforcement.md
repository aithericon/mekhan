# Runtime Port Enforcement — Typed Tokens Inside the Net

Status: Proposal
Author: handoff doc — continuation of [`05-typed-ports.md`](./05-typed-ports.md) and [`06-triggers.md`](./06-triggers.md). Addresses the gap surfaced while explaining how the type system maps to the engine.
Related: `service/src/compiler/compile.rs`, `service/src/models/template.rs` (`Port::validate_token`, `PortValidationError`), `service/src/petri/instance.rs`, `engine/sdk/src/context.rs`

## 1. Problem

Typed ports are enforced at **two places only**: compile-time static analysis (`validate_edges_typed`, `compile.rs:557`) and the system boundary (`Port::validate_token` at instance creation `petri/instance.rs:148` and trigger fire `triggers/dispatcher.rs:350`). Between those, **the running net is untyped.** Every workflow-body place is `PlaceHandle<DynamicToken>` (`compile.rs:1092` and throughout); the engine's colored-token capability (`token_schema` via schemars, `engine/sdk/src/context.rs:354`) is used only for engine infra plumbing, never for user `Port` schemas.

The consequence is a real correctness hole at **internal node boundaries**. An `AutomatedStep` declares an `output: Port`, but its terminal transition `t_{id}_to_output` (`compile.rs:1408`) is `logic("#{ output: done }")` — the executor's actual return value is passed through verbatim into `p_output` as raw `DynamicToken`, validated against the declared port **nowhere**:

- Not at compile (the Python/HTTP/LLM script body is opaque).
- Not at runtime (no `Port::validate_token` on executor results — it is only ever called in `petri/instance.rs` and `dispatcher.rs`).
- Not at the next place (`DynamicToken`, no schema).

Same for `HumanTask` form output. A step declaring `output: {result: json, score: number}` that actually returns `{"oops": true}` sails through; the mismatch only surfaces — if ever — when some downstream Decision guard dereferences the missing field and Rhai errors with an unattributable message. "Typed ports" today means *typed at the doors, dynamic in the hallways.* This proposal puts teeth on the internal doors that face opaque runtime code.

## 2. Goals & Non-Goals

**Goals**

- A token produced by a node whose output is synthesized from **opaque runtime input** (`AutomatedStep` executor result, `HumanTask` form submission) is validated against that node's declared output `Port` *before it leaves the node*, with the violation attributed to the producing node.
- The runtime check encodes **exactly the same rule** as `Port::validate_token` (`template.rs:645`) and `validate_edges_typed` — one notion of "valid" across static, boundary, and in-net checks. No drift.
- A violation is **loud and precisely diagnosed** (which node, which port, which field, expected kind vs. got), not a silent dead token or an unattributable downstream Rhai error.
- Bounded AIR cost: emit a guard only where it closes the actual hole, not at every port on every node.

**Non-goals**

- Not making the whole net a colored Petri net. Places stay `DynamicToken`; this is a *targeted gate*, not a type system retrofit of the runtime.
- Not validating structurally-identity nodes. `Decision`/`ParallelSplit`/`ParallelJoin`/`Loop` pass the token through with identity logic (`#{ output: input }`, `compile.rs:1286`); they synthesize no new shape and inherit their input's validity. No guard there.
- Not re-validating `Start` output in-net. Start's token is seeded by `parameterize_air`, which already runs `Port::validate_token` at the boundary (`petri/instance.rs:148`). Double-checking it inside the net is redundant.
- Not changing the executor/human protocols. We validate the value *after* it crosses back into the net, not by constraining how it was produced.

## 3. Design

### 3.1 Where the guard goes — and why not "on the edge"

The intuitive answer ("add a `guard_rhai` to the edge transition") doesn't work, because of how `wire_edge` (`compile.rs:1673`) actually emits AIR. Three cases:

1. `wiring_logic(target)` is `Some` → a real `t_edge_{id}` transition with `logic_rhai`.
2. Pure pass-through, single incoming edge → **place merge** (`compile.rs:1732`): `p_target_input` is *aliased away* to the source's output place. **No transition exists.**
3. Pure pass-through, multi-input non-join → a pass-through `t_edge_{id}` with `logic("#{ output: input }")`.

Case 2 is the common one, and it has no transition to carry a guard. Forcing a transition there to host a guard would defeat the merge optimization graph-wide.

So the guard goes **inside `expand_node`, on the producing node's output boundary**, not on the edge. Concretely, for the targeted node kinds, the existing "to output" transition is split into a validating fork.

### 3.2 The validating fork

Today (`AutomatedStep`, `compile.rs:1408`):

```
[lc.completed] --t_{id}_to_output (#{output: done})--> (p_output)
[lc.dead_letter] --t_{id}_to_error (#{error: dead})--> (p_error)
```

Proposed — replace the single `to_output` with two competing transitions out of `lc.completed`:

```
                 t_{id}_output_ok    guard: port_valid(done)   logic: #{output: done}   --> (p_output)
[lc.completed] --<
                 t_{id}_output_bad   guard: !port_valid(done)  logic: #{error: <diag>}  --> (p_error)
```

`p_error` already exists for `AutomatedStep` and already drains to the node's error output. A port violation becomes a *first-class, routed error* — same machinery as an executor failure, but with a structured diagnostic token instead of a dead token. A bare `guard_rhai` with no `else` arm was rejected precisely because it produces a stuck token (silent), which is the failure mode we are trying to kill.

`HumanTask` (`compile.rs:1312` `t_{id}_finalize`) gets the same treatment; it has no `p_error` today, so it gains one (and a node "error" output handle, consistent with `AutomatedStep`'s `output_places` convention at `compile.rs:1260`).

### 3.3 The guard predicate — generated from the `Port`, identical to `validate_token`

`Port::validate_token` (`template.rs:645`) defines validity via `PortValidationError::{NotObject, MissingRequiredField, FieldKindMismatch}`. The generated Rhai must be that rule, field-for-field. Mapping `FieldKind` → Rhai `type_of`:

| `FieldKind` | Rhai predicate on present field `v` |
|---|---|
| `Text`/`Textarea`/`Select`/`File`/`Signature`/`Timestamp` | `type_of(v) == "string"` |
| `Number` | `type_of(v) == "i64" \|\| type_of(v) == "f64"` |
| `Bool` | `type_of(v) == "bool"` |
| `Json` | *(no check — escape hatch, matches `kinds_compatible` Json rule at `compile.rs:680`)* |

Required vs optional: a required field must be present and kind-correct; an optional field, if present, must be kind-correct (mirrors `validate_token`). Object-ness: the token must be a map (`PortValidationError::NotObject`). A port with **no fields** or **all-`Json` fields** generates no guard at all — the validating fork is not emitted (consistent with the `tgt.fields.is_empty()` skip at `compile.rs:633`).

To guarantee one-rule-no-drift, the predicate is produced by a single function — `Port::to_validation_rhai(&self) -> String` — living next to `validate_token` in `template.rs`, with a test asserting that for a corpus of `(Port, token)` pairs, `validate_token(token).is_ok()` ⇔ the generated Rhai evaluates true. This makes the three checkers (static `validate_edges_typed`, boundary `validate_token`, in-net generated guard) provably the same predicate.

### 3.4 The diagnostic token

`t_{id}_output_bad`'s logic emits a structured error so the failure is attributable:

```rhai
#{ error: #{
    kind: "port_violation",
    node_id: "<id>",
    port: "<port_id>",
    detail: <first failing field + expected kind + got type_of>,
    value: done            // the offending payload, for debugging
} }
```

This rides the existing error topology to the node's error output / lifecycle. The lifecycle listener (`service/src/lifecycle.rs`) should recognize `kind == "port_violation"` and surface it on the instance as a terminal failure with the `detail` string, rather than a generic "errored." (Small follow-up in `lifecycle.rs`, not strictly required for the AIR change to be correct.)

### 3.5 Retry interaction (important, easy to get wrong)

`AutomatedStep` has a retry topology (`build_retry_topology`, around `compile.rs:1390`) that re-dispatches on executor failure/timeout. A **port violation is deterministic** — re-running the same script with the same inputs yields the same bad shape. Routing a port violation through the retry path would burn the retry budget pointlessly and delay the loud failure. Therefore `t_{id}_output_bad` must drain to `p_error` **directly**, bypassing `build_retry_topology` (which only consumes `lc.failed`/`lc.timed_out`/`lc.effect_errors`, not the new bad-output path — so this falls out naturally as long as the bad path is wired to `p_error`, not back into the lifecycle inbox). Call this out in the implementation so nobody "helpfully" unifies the two error sources.

## 4. Scope: which nodes get a guard

| Node | Output synthesized from opaque runtime input? | Emit guard? |
|---|---|---|
| `AutomatedStep` | Yes — executor result | **Yes** (primary target; `p_error` already exists) |
| `HumanTask` | Yes — form submission | **Yes** (gains a `p_error` + error handle) |
| `Start` | No — seeded via `parameterize_air`, boundary-checked already | No |
| `Decision`/`ParallelSplit`/`ParallelJoin`/`Loop` | No — identity pass-through / structural | No |
| `End` | Consumes only; `terminal` port | No (covered by upstream producers; optionally a debug assertion) |
| `Scope` | Boundary of a sub-graph | Deferred — see §7 |

This scoping is the cost control: in a typical graph only `AutomatedStep`/`HumanTask` nodes gain one extra transition each (and one place for `HumanTask`). No graph-wide bloat.

## 5. Phasing

**Phase 1 — `AutomatedStep` only.** The hole the user actually flagged. `Port::to_validation_rhai` + the equivalence test, the validating fork at `t_{id}_to_output`, bad-output drains to existing `p_error`, retry bypass verified. Ship behind nothing — if a step declares a typed output, it is enforced. (~1 week incl. compiler tests.)

**Phase 2 — `HumanTask`.** Add `p_error` + error output handle to `HumanTask`'s `NodePorts`, same fork at `t_{id}_finalize`. (~3 days.)

**Phase 3 — lifecycle surfacing.** `lifecycle.rs` recognizes `kind: "port_violation"`, marks the instance failed with the `detail` message and the producing node id. Editor instance view shows it. (~2 days.)

**Phase 4 — equivalence hardening.** Property test (`proptest`) generating random `Port`s + tokens, asserting `validate_token` ⇔ generated Rhai for all three checkers. Promotes "we think they match" to "they cannot diverge without a test failing." (~2 days.)

## 6. Open Questions

1. **`p_error` with no consumer.** Today an `AutomatedStep` with no error edge lets `p_error` dead-end (`compile.rs:1258` comment). If a port violation routes there and nothing consumes it, we've recreated a silent dead token. Recommendation: when a guard is emitted, also mark `p_error` as a **terminal** place (so the instance terminates as failed via lifecycle) even when the author drew no error edge. Decide before Phase 1.
2. **Number kind precision.** Rhai distinguishes `i64`/`f64`; JSON does not. `validate_token`'s current `Number` rule must be checked — if it accepts any JSON number, the Rhai must accept both `i64` and `f64` (table in §3.3 does). Confirm `validate_token` doesn't additionally reject non-integer for an integer-y field, or the two will diverge.
3. **`Json` fields and partial ports.** A port `{result: json, score: number}` only guards `score`. Intended (Json is the escape hatch) — but document it so authors know declaring `json` opts that field out of runtime enforcement, same as it opts out of static `kinds_compatible`.
4. **Strictness escape hatch.** Should an author be able to mark an output port "advisory" (static-checked, not runtime-gated) for migration of existing templates whose scripts don't yet conform? Recommendation: no new flag — `Json` already is the per-field escape hatch, and a fields-less port is the whole-port one. Adding a strictness toggle re-introduces the "config accepted but not enforced" smell from `06-triggers.md` §3.4.
5. **Existing templates.** Republishing an existing template with a non-conforming script + declared typed output will now fail at runtime where it previously silently corrupted. That is the point, but it is a behavior change for live workflows. Recommendation: a one-time audit pass that compiles every published template, dry-runs the generated predicate against recent historical executor outputs from the catalogue, and reports which would now fail — so the breakage is discovered before rollout, not in production.

## 7. Out of Scope

- **`Scope` sub-graph boundary enforcement.** When `Scope` blocks become invokable sub-templates with declared port interfaces (deferred in `05-typed-ports.md`), the same fork pattern applies at the scope boundary. Not until sub-templates are real.
- **Colored places / `token_schema` in AIR.** Genuinely typing the places (using the engine's schemars capability for user ports) is a larger architectural change with runtime-validation implications across the engine. This proposal is deliberately the cheap, contained version that closes the practical hole.
- **Input-side validation.** Validating a token on *entry* to a consuming node (vs. exit from the producer). The producer-side check catches the same corruption with correct attribution; input-side would be redundant and mis-attribute. Revisit only if a node can receive tokens from sources the producer-side guard doesn't cover.

## 8. Acceptance Criteria

**Phase 1 ships when:**
- `Port::to_validation_rhai` exists beside `validate_token`, with a test asserting predicate equivalence over a fixed `(Port, token)` corpus including each `PortValidationError` variant.
- An `AutomatedStep` declaring `output: {result: json, score: number}` whose executor returns `{"result": {...}}` (missing `score`) terminates the instance as **failed** with a message naming the node, port, and field — verified by a compiler + e2e test.
- The same step returning a conforming token completes normally; the added fork is inert on the happy path.
- A port violation does **not** consume the retry budget (test asserts retry count unchanged).
- Ports with no fields or all-`Json` fields emit no extra transition (AIR snapshot test).

**Full proposal ships when:**
- `AutomatedStep` and `HumanTask` both enforce declared output ports at runtime.
- A port violation surfaces in the editor instance view with node/port/field detail.
- The property test makes static `validate_edges_typed`, boundary `validate_token`, and the in-net generated guard provably one predicate.
- The pre-rollout audit pass exists and has been run against the current published-template corpus, with results reviewed.
