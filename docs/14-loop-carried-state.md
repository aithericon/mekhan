# 14 — Loop carried-state lifecycle

**Status:** Design (recommended). Part A (carried-state envelope) is a clear
bug fix; Part B (post-loop body output) is gated behind one product call.

**Scope:** how loop-body parked places are lifecycled across iterations, so
that (c) `input.<Start field>` and other control-token leaves survive a
token-stripping body, and (b) post-Loop scope can read the body's final
output without forcing the stop test into `loop_condition`.

Read first: [`10-control-data-token-model.md`](./10-control-data-token-model.md)
(the borrow/control-data model this design must stay consistent with — §9
flags this exact gap: "Per-iteration loop-produced data keying … is out of
scope"). This doc closes it.

## 1. Symptoms (from live e2e)

| # | Symptom | Status |
|---|---|---|
| (a) | `maxIterations` bypassed when the body strips the counter leaf off the control token | **Already fixed** — counter is parked, not token-resident |
| (b) | Post-Loop scope cannot see body output → a stop condition MUST live in `loop_condition`, not `maxIterations` or a post-Loop Decision | **Open** |
| (c) | `input.<Start field>` does not survive a token-stripping body inside a loop — control-token leaves not re-emitted each iteration vanish | **Open** |

## 2. Why each symptom happens (current code)

### (a) is already fixed — do not re-litigate

`lower_loop` parks the iteration counter in a write-once-per-iteration
envelope at `p_{id}_data` (`service/src/compiler/lower/loop_.rs:88-91`) and
pre-wires the continue/exit transitions to bind it via a `d_<slug>` port
(`loop_.rs:124-148`). The continue guard is authored as
`{slug}.iteration < {max_iterations} && ({loop_condition})` (`loop_.rs:129-130`);
the standard read-arc synthesis pass rewrites `<slug>.iteration` →
`d_<slug>.iteration` against the parked place (`resolve_ref` Loop arm,
`service/src/compiler/borrow/planners/guard.rs:210-231`;
`apply_guard_borrows`, `service/src/compiler/borrow/apply/guard.rs:62-66`).
The counter therefore survives any body — including an AutomatedStep whose
executor envelope strips the workflow token. The MEMORY note describing (a)
predates this refactor.

### (c) — control-token leaves die in a token-stripping body

The loop "carries" state across iterations ONLY by re-emitting the
counter/accumulator envelope. The control token threaded through the body is
forwarded *verbatim*:

- enter: `#{ body: input, data: #{ iteration: 0 <acc_enter> } }` (`loop_.rs:114-116`)
- continue: `#{ body: input, data: #{ iteration: {slug}.iteration + 1 <acc_continue> } }` where `input` is consumed from `p_body_out` (`loop_.rs:124-134`)

When the body is an AutomatedStep, the token is slimmed to identity/routing
keys only:

- `YIELD_LOGIC` keeps `_*` / `task_id` / `status` (`service/src/compiler/token_shape/surface.rs:67-69`)
- the executor lifecycle's `t_success` keeps only `_`-prefixed leaves (`engine/sdk/src/components/executor_lifecycle.rs:163-179`)

So any non-`_` control-resident leaf present at loop entry (Start fields, a
constant the body needs each pass) is gone from `p_body_out` onward. The
continue transition forwards the slimmed token back into `p_body_in`, and
iteration 2 sees `input.<startfield>` = undefined. The current parked-counter
mechanism is exactly the special-case fix for the SAME problem applied only
to the counter.

### (b) — post-loop scope can't see body output

`lower_loop` publishes only `interface.data_port = p_{id}_data`
(`loop_.rs:173`), which holds `{ iteration, <accumulators> }` — NOT the
body's final output. The loop's outer output place `p_{id}_output` carries
the slim final-iteration control token (`#{ output: input }`, `loop_.rs:147`),
and a body AutomatedStep's business output is parked in the BODY's own
`p_{bodyslug}_data`. `resolve_ref` rejects a post-loop borrow of
`<bodyslug>.<field>` because the body child sits topologically AFTER the loop
and the only allowed downstream-borrow exception is *consumer == Loop reading
its own body child* (`guard.rs:249-262`). So a post-Loop Decision or End
mapping cannot borrow it — forcing the stop test into `loop_condition`
(which CAN reach body output via the same body-child exception).

## 3. Design principles this must honour

- **No new place class.** The loop already owns one parked place
  (`p_{id}_data`); generalize it rather than inventing a parallel place.
- **No Start special-case.** A Start field is just a non-`_` entry leaf;
  the mechanism must carry *any* such leaf, not pattern-match Start.
- **One resolver, no drift.** The picker, diagnostics, and read-arc
  synthesis all flow through `resolve_ref` (docs/10 §5). Anything the body
  can reference must resolve there.
- **`_`-prefixed reserved keys.** Reserved envelope keys ride the surviving
  `_` metadata channel and never collide with business leaves or with a
  nested loop's envelope. Use `_loop_carried` / `_loop_result`.
- **No engine change.** Read-arc newest-token semantics already pick the
  newest per-iteration park (`engine/core-engine/crates/application/src/binding.rs:310-322`),
  and the monotone parked-place model already supports per-iteration
  re-park. The fix is compiler-only.

## 4. Part A — carried-state envelope (closes c)

> **Implementation note — port names are id-derived, NOT human slugs.** This
> doc writes `d_<slug>` for readability, but the physically-wired port is
> derived from the node **id**, not the human `slug`:
> `d_{id.replace('-','_')}` (`loop_.rs:71`), and the read-arc borrow var is
> likewise `d_{producer_node.replace('-','_')}` (`apply/guard.rs:31`). When
> implementing, emit the **id-derived** `d_…` name physically wired at
> `loop_.rs:126` — do NOT interpolate the human slug. Copying the literal
> `d_<slug>._loop_carried` with `<slug>` = the human slug would reference a
> nonexistent port and silently resolve `_loop_carried` to `()` every
> iteration, reintroducing exactly bug (c).

Generalize `p_{id}_data` from `{ iteration, <acc> }` to ALSO hold every
control-token business leaf the body needs each pass, under a reserved
`_loop_carried` sub-key. Re-emit it unchanged on every continue, and MERGE
it back onto the body token handed to `p_body_in`.

### 4.1 IR shape

**enter** — snapshot the non-`_` business leaves into `_loop_carried`,
hand the full token to the body unchanged (iteration 1 still has every
entry leaf on the token directly):

```rhai
let __c = #{};
for k in input.keys() {
    if !k.starts_with("_") && k != "task_id" && k != "status" { __c[k] = input[k]; }
}
#{ body: input, data: #{ iteration: 0 <acc_enter>, _loop_carried: __c } }
```

**continue** — overlay `_loop_carried` onto the slimmed body-out token
(body-out wins for keys it actually emitted; carried fills only gaps), and
re-emit the snapshot unchanged:

```rhai
let __b = input;                      // body-out token (slimmed by an AutomatedStep)
let __c = d_ID._loop_carried;         // d_ID = id-derived port (see note above), consumes p_data
for k in __c.keys() { if __b[k] == () { __b[k] = __c[k]; } }
#{ body: __b, data: #{ iteration: <slug>.iteration + 1 <acc_continue>, _loop_carried: __c } }
```

(`d_ID` above is shorthand for the id-derived `d_{id.replace('-','_')}` port,
per the implementation note at the top of §4 — not the human slug.)

`d_<slug>._loop_carried` is already physically available: the continue
transition consumes `p_data` through the pre-wired `d_<slug>` input port
(`loop_.rs:126`). `apply_guard_borrows` leaves that pre-wired arc alone
(`allow_under_consume_arc = false`, `apply/guard.rs:58-62`), so referencing
`d_<slug>._loop_carried` directly in the hand-authored continue logic is
safe — it is NOT a synthesized borrow, it is a literal port reference (same
as the existing `<slug>.iteration + 1` which IS synthesized; `_loop_carried`
is hand-wired because we know the port name here).

**exit** — unchanged for Part A (`#{ output: input }`, `loop_.rs:147`); Part
B extends it.

### 4.2 Why this closes (c) with no Start-awareness

A Start field `invoice_id` is a non-`_` leaf on the inbound control token.
At enter it lands in `_loop_carried`. The body's AutomatedStep strips it
off `p_body_out`, but continue re-overlays it from `d_<slug>._loop_carried`
before handing the fresh token back to `p_body_in`. Iteration 2 sees
`input.invoice_id` again. No code path mentions Start.

### 4.3 Scope/type plumbing

The loop body child's `node_in` is the shallow-merge of its DAG
predecessor's outbound shape (`analyze.rs:559-569`); the predecessor is the
loop's `p_body_in` handle, whose shape is `out_shape_loop`'s `o =
in_shape.clone()` plus the `<slug>` namespace (`analyze.rs:447-472`). So the
inbound business leaves are ALREADY visible in the body's `node_in`, and
`input.<field>` inside the body resolves as `Control` through the existing
`resolve_ref` Input arm (`guard.rs:158-172`). A body `input.<field>` NOT
present at loop entry already hard-errors: `resolve_ref` returns
`Unresolved` → `GuardUnresolved` at publish (`guard.rs:654-665`). **No
`analyze()` change is required for v1.** (If a future refactor makes the
body_in handle shape diverge from the inbound token, this is the seam to
union the carried leaves explicitly — call it out then, don't add it
speculatively.)

### 4.3b AutomatedStep Python-body staging — the load-bearing case (VERIFY)

§4.3 establishes that a **guard/Rhai-resolved** `input.<field>` reference (the
path used by loop conditions and Rhai guards) resolves through `resolve_ref`'s
Input arm. But the primary live case — the BO demo bodies — is a **Python**
AutomatedStep that reads Start fields via **direct-slug access** (`a =
review.invoice_id`), which the compiler handles in a SEPARATE planner
(`automated_step.rs`, the source-scan that synthesizes read-arcs + stages
`<slug>.json` and promotes each to a Python global; see the "Direct slug
access" model). Carrying the leaf back onto the `p_body_in` token (Part A) is
**necessary but not proven sufficient** for that path: it is not yet verified
that the AutomatedStep input-staging step re-reads the re-merged token on
iteration 2+ and re-promotes the carried leaf to a Python global. **Before
implementing Part A, trace `automated_step.rs`'s input-staging to confirm the
body's executor input snapshot is rebuilt from the (re-merged) inbound token
each iteration**, not captured once. If staging reads a parked place rather
than the live `p_body_in` token, Part A must also re-park the carried leaves
where that planner reads them. This is the single most important thing to
validate on the live e2e (§9) — token-carries-the-key ≠ body-code-can-read-it.

### 4.4 Carry-on-conflict semantics

When the body re-emits a key also present in the entry snapshot, the
body's fresh value wins for the next iteration (the `if __b[k] == ()`
overlay only fills gaps). Rationale: a body that genuinely mutates a
carried value (rare; usually you'd use an accumulator) gets its mutation
through; a body that strips the key (the common AutomatedStep case) gets
the immutable entry snapshot back. Authors who want strictly-immutable
carry across a mutating body use an accumulator instead — that is the
blessed stateful-carry surface.

### 4.5 Snapshot scope

Snapshot ALL non-`_` entry leaves, not just statically-referenced ones.
Snapshot-all is fully general and bounded (the entry token's leaves are
themselves bounded by Start's declared fields, already write-once parked),
and it avoids reintroducing a source-scan dependency (counter to the
"declared over inferred contracts" preference). The cost is a slightly
fatter parked token; acceptable.

## 5. Part B — expose final body output post-loop (closes b)

**This part is gated behind a product call (§7).** Two viable shapes:

### B1 (recommended if we ship per-iteration final output)

`t_{id}_exit` read-arcs the body's last parked output (`p_{bodyslug}_data`,
newest token via `binding.rs:318`) and folds it into the loop's published
`p_{id}_data` under `_loop_result`. `resolve_ref` then resolves
`<loop_slug>.result.<field>` post-loop as a plain upstream Loop borrow.
This keeps the loop's published contract self-contained (one `data_port`)
rather than exposing per-iteration body places as a stable downstream
contract.

Exit IR (extends `loop_.rs:140-148`):

```rhai
// exit reads both the parked counter AND the body's final parked output,
// folds the final body output under _loop_result into the published envelope.
// read_input d_<slug> on p_<id>_data (counter) stays; ADD a read-arc on
// p_<bodyslug>_data and re-park the merged envelope.
#{ output: input,
   data: #{ iteration: <slug>.iteration, _loop_result: d_<bodyslug> } }
```

Functions that change for B1:
- `lower_loop` exit transition: add a `read_input` on the body child's
  parked place and re-emit `p_{id}_data` with `_loop_result`. lower_loop
  knows its body children (`cx.children`), so the body slug/data place is
  in hand.
- `out_shape_loop` (`analyze.rs:447`): add a `result` field to the `<slug>`
  namespace shaped from the body child's declared output port, so the
  picker offers `<loop>.result.<field>`.
- `resolve_ref` Loop arm (`guard.rs:210-231`): **this needs a real
  producer_path remap, not a confirmation.** The arm currently joins
  `gref.segs` verbatim (`producer_path = gref.segs.join(".")`), so segs
  `[result, field]` yield the physical path `result.field` — but the parked
  envelope stores the value under `_loop_result.field`. B1 must remap the
  `result` logical sub-namespace to the physical `_loop_result` path (a
  logical/physical divergence the Loop arm does not currently model, unlike
  the human-task `data.` phys-path handled by the generic non-loop arm via
  `find_by_leaf` at `guard.rs:317`). Plan this as an explicit change to the
  Loop arm plus a matching phys-path on `out_shape_loop`'s `result`
  namespace — "confirm it resolves" understates it.

### B2 (rejected)

Widen the body-child downstream exception (`guard.rs:249-262`) to admit
post-loop consumers borrowing `p_{bodyslug}_data` directly. Leakier: it
exposes an internal per-iteration place as a stable downstream contract and
muddies "the loop's output is its published envelope." Reject in favour of
B1.

## 6. Functions that change (summary)

| Function | File | Change |
|---|---|---|
| `lower_loop` enter logic | `service/src/compiler/lower/loop_.rs:110-117` | snapshot non-`_` leaves into `_loop_carried` |
| `lower_loop` continue logic | `loop_.rs:124-135` | overlay `_loop_carried` onto body-out, re-emit it |
| `lower_loop` exit logic | `loop_.rs:140-148` | **(B1 only)** read-arc body parked output → `_loop_result` |
| `out_shape_loop` | `service/src/compiler/token_shape/analyze.rs:447-473` | **(B1 only)** add `result` to `<slug>` namespace |
| `resolve_ref` Loop arm | `service/src/compiler/borrow/planners/guard.rs:210-231` | **(B1 only)** confirm `<loop>.result.<field>` resolves |

NO engine change. `guard_readarc_plan` needs no new arm — `<slug>.iteration`
and `input.<field>` are resolved by existing arms; `_loop_carried` /
`_loop_result` are hand-wired literals in lowering, not author-written refs.

## 7. Open product call (gates Part B)

Does a post-Loop consumer want the **final iteration's** body output, or is
the **accumulated collection** (which accumulators already provide) the
blessed across-iteration result surface? If accumulators are blessed, Part B
is DOC-only — "use an accumulator for post-loop results" — and only Part A
(the clear bug) ships as IR. Recommendation: ship Part A immediately; make
the Part B call before adding `_loop_result` IR.

## 8. Open questions / risks

- **Nested Loop/Map collisions:** an inner loop's `_loop_carried` /
  `_loop_result` / `iteration` vs. an outer's. The `_loop_` prefix keeps
  them off the business-leaf namespace, but two nested loops both park
  `_loop_carried` in *their own* `p_{id}_data` — distinct places, no
  collision. A business leaf literally named `_loop_carried` is impossible
  (it is `_`-prefixed, so it would be filtered out of the snapshot and
  treated as routing metadata anyway). Confirm with a nested-loop test.
- **Map-in-Loop (demos/12-bo-loop):** confirm `_loop_carried` survives BOTH
  the Map body's slimming AND the loop continue, and that newest-token
  read-arc picks the right per-iteration park under concurrent body places.
  This is the primary live e2e gate (see §9).
- **Fatter parked token:** snapshot-all widens `p_{id}_data`. Bounded by
  Start's declared fields; acceptable, but watch the schema validation
  boundary (`Data__{id}` def) — `_loop_carried` is an opaque sub-object
  (`Any`/permissive), consistent with the documented strict-ramp (docs/10
  §6/§9). **Confirm `Data__{id}` actually admits an arbitrary-shape
  `_loop_carried` sub-object**: `compile.rs` parks `p_{id}_data` as an open
  object for the small dynamic `_loop_*` keys, but if `Data__{id}` is a
  closed object the per-iteration snapshot fails runtime output validation.
- **Loop-bearing regression targets:** the demos that actually exercise loops
  in this checkout are **`demos/12-bo-loop`** (Map-in-Loop) and
  **`demos/12a-bo-catalog-trigger`** — regression-test both. (Note: `demos/13`
  here is `13-dynamic-form`, not loop-bearing; the resource-pool net lives in
  a separate unmerged worktree, not in scope for this doc's regression set.)
- **AIR-snapshot churn:** Part A changes the enter/continue logic of EVERY
  loop, so all committed `air_snapshots` covering loops will change — expect
  to regenerate those snapshots, not assert they're byte-unchanged.

## 9. Verification

### Offline (compiler)

- `cargo test -p mekhan-service` (built from the umbrella root → `./target/`).
- AIR snapshot diff: enter/continue logic for a Loop with a Start field
  carried into an AutomatedStep body now contains the `_loop_carried`
  snapshot + overlay; no other transition changes.
- A unit test asserting a body `input.<field>` present at loop entry
  resolves `Control` (no `GuardUnresolved`), and one NOT present still
  hard-errors.

### Live e2e (required — see open questions)

- `demos/12-bo-loop` (Map-in-Loop): run end-to-end on live dev; confirm
  iteration 2+ sees the carried Start field and the loop terminates on
  `maxIterations` with the body having stripped the token each pass.
- (B1 only) a post-Loop Decision borrowing `<loop>.result.<field>` fires
  on the final iteration's body output.
