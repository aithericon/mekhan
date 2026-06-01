# 19 — Shape as a Typed Affine IR for the Workflow Compiler

**Status:** exploration / design dialogue. No code written. Captures a multi-turn
analysis of whether the Shape language's compiler + type-resolution infrastructure
could be married into the petri-net workflow compiler — culminating in a concrete
"post-AIR" architecture and a staged path that starts by replacing Rhai.

**Date:** 2026-05-30 (conversation), this doc 2026-06-01.

**Cross-refs:** `docs/10-control-data-token-model.md` (the borrow/token model),
`docs/14-loop-carried-state.md`, `docs/12-agent-node-design.md` (engine effect
journaling / replay), `docs/05-typed-ports.md`, `docs/07-runtime-port-enforcement.md`.
Shape repo: `/Users/milanender/AithericonResearch/shape`.

---

## 0. TL;DR

- The petri compiler's `TokenShape` is **not a type system** — it is structural +
  provenance (a borrow checker over tokens). Shape, by contrast, is a real
  statically-typed language with HM-style inference, unification, bidirectional
  checking, unions/generics, and content-addressed bytecode.
- The two solve **orthogonal** problems: Shape *infers types of values*; the petri
  compiler *proves where a field came from and wires a read-arc*. The hardest,
  most valuable part of our compiler (provenance + read-arc synthesis) has **no
  analog in Shape**.
- But once you account for **control-flow composition** (Decision joins, Loop
  fixpoints, Map generics, failure sum-types, scopes), there *is* a real,
  currently-unsound type-flow problem — and Shape's **type lattice + algorithms**
  are a strong fit for it. Two of our live, recurring bugs (Map-collect all-null;
  Agent-SubWorkflow-tool collect-empty) are eliminated *by construction* in a
  value-semantics IR.
- **Key theory:** a petri net is representable as a *normal* (intuitionistic)
  function graph only partially. As a **linear/affine** function graph (Rust-like:
  ownership + `&`/`&mut`) it is faithful **for the well-structured workflow-net
  subclass**. Our arc semantics *already are* an affine ownership discipline:
  parked place = owned value, read-arc = `&`, consume = move, resource-pool place =
  `&mut`/`Arc<Mutex>`. Petri nets are a model of linear logic.
- **Post-AIR vision:** Shape becomes the platform IR; AIR (untyped JSON) is
  replaced. The visual graph stays the authoring surface (Shape-as-IR ≠ users
  writing Shape). The petri net demotes to a *compiled execution target*
  (Architecture A, engine unchanged) or eventually a *structural view*
  (Architecture B, Shape VM is the runtime).
- **First move:** replace Rhai. Do **not** build a throwaway Shape→Rhai transpiler
  ("limited Phase 0"). Commit to the engine executing Shape. Guards (pure,
  replay-trivial) are the clean first cut; the plumbing Rhai should be **obviated
  by the typed IR**, not ported.
- **Two facts still to verify** before scoping: (a) is net control flow
  *well-structured by construction* (`graph.rs`/`lower/`/`wire.rs`)? (b) is guard
  evaluation separable from plumbing eval at the engine's Rhai eval site?

---

## 1. The two systems, mapped

### 1.1 Shape's type infrastructure (`/Users/milanender/AithericonResearch/shape`)

- `shape-ast` — `TypeAnnotation` (Basic/Array/Tuple/Object/Function/Union/
  Intersection/Generic/Reference/Dyn/…), `TypePath`, `StructTypeDef`,
  `FieldAnnotation` (`@description`/`@range`/`@example`). Standalone, no VM deps.
- `shape-runtime/type_system` — real `Type` enum (`Variable(TypeVar)`, `Generic`,
  `Constrained`, `Function`), `TypeScheme` (∀-quantified + trait bounds),
  `Unifier` (Robinson unification + substitution), `TypeInferenceEngine`
  (bidirectional `Infer`/`Check`/`Synth`, property-access inference,
  flow-sensitive narrowing), constraint solver. ~80% liftable out of the VM with
  soft couplings (record-schema env, method table, trait resolver).
- `shape-runtime/type_schema` — `TypeSchema`/`FieldDef`/`FieldType` registry with
  per-field annotations. Decoupled from inference.
- **End-to-end inference is real on HEAD** (colleague-verified): `f5(f4(f3(f2(f1()))))`
  with zero annotations compiles; a 16-round call-graph fixpoint
  (`apply_callsite_unions`) propagates types caller↔callee. "Annotate the
  boundaries, infer the glue" is exactly what it does — *for a Shape program*.
- **Gaps:** `@ai fn -> ReturnType` → JSON-Schema structured output is **designed
  but not implemented** (annotations are parsed/stored, never lowered). No `any`
  type by design (inference failure is a hard error).

### 1.2 The petri compiler (`service/src/compiler/`)

- **`TokenShape`** (`token_shape/types.rs`): `Object/Array/Scalar/Any/Opaque`;
  `ScalarTy = String|Number|Bool|FileRef|Timestamp|Json`. Each `Field` carries
  `Provenance { node_id, node_label, note, anchor }`. **Structural + provenance,
  not a type system.** No unions, generics, refinements, nominal typing. One
  `Number` (int/float conflated). `to_json_schema()` is permissive by design
  (`additionalProperties: true` — the "declared→enforced ramp").
- **The single resolver** `resolve_ref()` (`borrow/planners/guard.rs:148-332`):
  resolves `<slug>.<field>` → producer place + path + type via `SlugIndex`
  (slug→node), `node_out: BTreeMap<id, TokenShape>` (the type map), and
  `is_parked_producer`. Feeds **three** consumers from one source of truth:
  editor variable picker (`reachable_scope`), diagnostics (`check_guard`),
  read-arc synthesis (`guard_readarc_plan`).
- **No inference.** All ports explicitly declared; types flow by *lookup* in
  `node_out`, never unified. Multi-predecessor input = shallow merge (unsound at
  joins — see §3).
- **The borrow checker is the sophisticated part** (`borrow/`, `apply_control_data_foundation`
  in `compile.rs:946`): provenance-based ownership, upstream-only borrows, `read:
  true` non-consuming arcs, scope-by-provenance. Five scan surfaces (guards/Python/
  resource/human-task/backend) unified at the *apply* phase via `Borrow` +
  `BorrowResolution`, but each scans separately.
- **Rhai** (`rhai_gen.rs`/`rhai_scope.rs`): guards + transition logic. Hand-rolled
  string identifier walker (`extract_qualified_refs`), `__pluck`/`__set_path`
  helpers, placeholder interpolation. **Checked against nothing** → a type error
  can wedge a net.

### 1.3 The orthogonality

| | Shape | Petri compiler |
|---|---|---|
| Central hard problem | *Infer* the type of an unannotated expression | Prove *where a field came from* + wire a read-arc |
| Mechanism | HM unification, type vars, bidirectional check | Provenance `TokenShape`, `resolve_ref`, read-arc synth |
| Types inferred? | Yes (the whole engine) | No — ports declared, types looked up |
| Ownership / dataflow identity | None (types values) | The entire point (`Provenance` per field) |

Conflict to note: importing Shape's inference clashes with our stated value
**"declared over inferred contracts"** (downstream-read contracts stay explicit).
Shape's borrow checker analog does **not** exist — `&`/`&mut` is value-level, not
dataflow-provenance. So the borrow layer survives any marriage unchanged.

---

## 2. Where reuse actually fits (and where it doesn't)

Three grades of "reuse Shape's compiler infrastructure":

1. **Reuse the algorithms / type lattice (realistic).** `Union`/`Generic`, unify
   reduced to join/meet/subtype, scoped `TypeEnvironment`, flow narrowing — ported
   onto our provenance-carrying shapes, driven by our own graph traversal. A few
   hundred lines, no `shape-runtime` dependency.
2. **Import the crate (not realistic).** `analyze_program` is welded to a
   Pest-parsed *expression AST* + runtime registries. Our unit of composition is a
   **graph**, not an expression tree. Can't feed a petri net in.
3. **Adopt Shape as the body/guard language (the big bet).** Bodies + guards
   *written in Shape* → its inference runs natively inside each step, the graph
   layer composes those types across combinators. The only world where you reuse
   the compiler *wholesale*. Multi-month, strategic.

Native low-risk wins that don't need any import: add `Union` (for the
FileRef-or-string duality currently modeled as opaque `{}`), numeric refinements
(`> 5000` checked against a declared range), and a structural-subtyping check
("does producer A's shape satisfy consumer B's declared input?", ~50 lines).
JSON-Schema emission is already *ahead* of Shape (we have `to_json_schema`; Shape's
`@ai` schema path is unbuilt).

---

## 3. The combinator type-flow problem (why inference is real after all)

Per-step types are leaves (declared, lowered to petri). The **composition** is
where types must propagate, and it's currently approximated/unsound:

| Construct | Type-theory operation | Shape primitive | Current state |
|---|---|---|---|
| Decision + reconvergence | join/meet (sum for *possible*, meet for *guaranteed*) | `match` / `Union` | shallow predecessor-merge = unsound |
| Failure branch | `Result<T,E>` / `?` | `Result` + flow narrowing | not typed distinctly |
| Loop + accumulator | **fixpoint** `typeof(merge) ⊑ typeof(init)` | `loop { break v }` | accumulator typed `Any` (gave up) |
| Map / scatter-gather | generic `(T)->U` over `Array<T>` → `Array<U>` | `.map(closure)` | **live bug**: collect reads ctrl token not parked data |
| Parallel fork-join | product type | `join all` | — |
| Iterators | element binding into scope | bidirectional closure-param infer | — |

**Evidence this is the real problem area (from our own backlog):**
- Map-collect all-null gather, and Agent-SubWorkflow-tool collect-returns-empty
  (`project_agent_subworkflow_tool_collect_empty`, confirmed live) are the **same
  class** — `t_collect` reads the slim control token instead of parked data. In a
  value-semantics IR (`arr.map(|x| step(x))` returns a value) this **cannot
  exist** — there is no park/control split for the gather to read the wrong side.
- `project_loop_composition_gaps` ("post-Loop scope misses body output") is a
  **scoping** bug = body-local bindings escaping the block without being threaded
  through the exit.

**Critical distinction:** Shape's *type lattice + algorithms* fit this; Shape's
*inference driver* (expression-AST-directed) does **not**, because the graph is not
an expression tree. You drive Shape's lattice with your own graph dataflow/fixpoint
pass. Provenance rides on top either way → port algorithms, not crate. And the
check must run on the **WorkflowGraph pre-lowering** — after lowering, the
combinator structure is gone (just places/arcs; engine's `SchemaRegistry`
validates per-place, no cross-place reasoning).

---

## 4. Are petri nets representable as function graphs?

**Short:** not as *normal* function graphs; **yes** as *linear/affine* ones
(Rust-like), **for the well-structured subclass**.

### 4.1 Why a normal function graph is insufficient

1. **Linearity.** A function-graph value is freely copyable (intuitionistic); a
   token is *consumed exactly once* (linear). **Petri nets are a model of linear
   logic** (a transition is `A ⊗ B ⊸ C ⊗ D`). A normal graph erases consume-once.
   To be faithful the IR must track linearity → **affine/ownership types** (Rust).
2. **Unstructured nets have no structured equivalent.** Workflow-net theory (van
   der Aalst soundness; Kiepuszewski et al. structured-vs-unstructured) shows some
   unstructured split/join patterns (crossing, non-well-nested — the workflow
   `goto`) have **no** equivalent block-structured program without node
   duplication or auxiliary synchronization state.

### 4.2 The correspondence (our arcs already *are* ownership)

| Petri mechanism | Rust / affine type | We have it as |
|---|---|---|
| Parked write-once place `p_{id}_data` | **owned value** | producer data place |
| Read-arc (`read: true`, non-consuming) | **`&T` shared borrow** | read-arc synthesis |
| Consuming arc (weight 1) | **move** | control-token flow |
| Shared resource place (many consumers) | **`&mut` / `Arc<Mutex<T>>`** | resource-pool-net |

So the provenance + read-arc machinery is a **Rust-style borrow checker over
tokens**. "Add borrow/mut" names the discipline the arcs already encode; it's the
*enabling condition* for a faithful function-graph representation, not a feature
add.

### 4.3 The representable subclass + the danger zones

Representable = **well-structured sound workflow nets**: single entry/exit, every
AND-split matched by a unique AND-join, every XOR-split by a unique XOR-join,
well-nested (balanced parentheses). **Guaranteeable by construction** if the
combinator vocabulary only emits matched, single-entry-single-exit blocks
(Decision→match, Parallel→product, Map→`Array<T>`, Loop→structured iteration,
SubWorkflow→block).

Escape hatches that leave the structured world (type them explicitly, don't treat
as bugs):
- **Resource-pool-net** — shared mutable state w/ mutex; the `&mut`/`Arc<Mutex>` of
  the model. First place `&mut` is genuinely needed.
- **Unstructured N-of-M synchronization** (milestones, discriminators) — non-
  free-choice patterns with no structured equivalent.

### 4.4 Scoping falls out

Scope is well-defined **only** in a structured net (that's why ours is currently
underdefined). Then: **scope = the structured block.** A binding produced inside a
Loop body escapes only if threaded through the exit (the accumulator). The
loop-scope bug restated as the correct lexical-scoping law.

---

## 5. The post-AIR architecture

**Invariant:** Shape-as-IR ≠ users write Shape. The visual graph stays the
authoring surface and is *elaborated* into Shape IR (a compiler-internal
representation). AIR-the-untyped-JSON is what dies.

### 5.1 Today

```
WorkflowGraph ──mekhan compiler──▶ AIR (untyped JSON: places/transitions/arcs + Rhai strings)
                                     │
                                     ▼ POST /petri
                               core-engine (executes net, event-sourced) ──NATS──▶ executor
```

### 5.2 Architecture A — Shape is the IR, petri net is its *compiled target* (recommended first)

```
WorkflowGraph  ◀──── projection (editor renders it) ────┐
   │                                                     │
   ▼ elaborate                                           │
Shape IR  ═══ THE IR: typed · affine · content-addressed ╪══
   │   • guards/conditions = Shape expressions (Rhai gone)│
   │   • references         = owned/`&` bindings ─────────┘  ← borrow checker REFRAMED as Shape's affine check
   │   • steps              = typed effects → executor (body opaque, signature typed)
   ▼ lower (Shape ⊗/⊸ → places/transitions/arcs)   ← mechanical, *because* affine-net ↔ linear logic
Petri net (compiled operational form, AIR-shaped)
   ▼ POST /petri
core-engine (UNCHANGED) ──NATS──▶ executor
```

Where the switch happens, phase by phase:
- **Front half stays** (`graph.rs`, `validate.rs`, editor).
- **Middle flips:** `lower`/`wire`/`compile` emit **Shape IR** instead of AIR + Rhai.
- **`apply_control_data_foundation` / borrow checker is reframed, not replaced** —
  references elaborate to owned/`&` bindings; "upstream parked producer" = "in
  scope and owned"; read-arc = `&`; consume = move. Your checker computes the
  ownership half; Shape's checker adds the typing half.
- **`rhai_gen.rs` deletes** (guards become typed Shape expressions).
- **New back-half phase:** Shape IR → petri net (mechanical given linear-logic
  correspondence).
- **Soundness seam closes:** one lowering (typed→untyped), not two parallel ones,
  so the net executes exactly what was typed.

### 5.3 Architecture B — Shape is IR *and* runtime (north star)

```
WorkflowGraph ◀─projection─ Shape IR ═══ THE IR & runtime ═══
                               │
                               ▼ Shape VM executes directly
                         snapshot()/resume = event-sourcing · NATS = continuation handoff
                         Petri net = STRUCTURAL VIEW for the instance-graph UI only
```

Replaces the engine. `snapshot()`-at-await ↔ pause-at-transition; content handoff ↔
executor dispatch (Shape README sells exactly this). Do only after A proves the VM
as a runtime.

### 5.4 Invariants across both

1. **Shape is the *orchestration* IR, not the compute.** Step bodies (Python/
   Docker/HTTP) stay opaque foreign effects; Shape types their signature/boundary,
   the executor runs the interior. Sufficient — every current bug lives in
   orchestration *between* steps.
2. **Identity = content hash + human label.** Hash = true identity (dedup,
   integrity, Merkle dependency versioning for SubWorkflows — replaces by-value
   embedding at `template.rs:603`). Keep a `(name, semver) → hash` map; hashes
   aren't human-meaningful and leaf edits ripple the hash to all transitive callers
   (correct, not free — a re-publish avalanche for shared sub-flows).
3. **Capability-hash and Vault are complementary.** Permissions folded into the
   content hash + linker transitive union = a static, signed, tamper-evident
   manifest — but **blind inside opaque compute** (a `fn python`/Docker step has no
   statically-visible capability → over-grant or unsound). Vault stays for secret
   custody + the one thing a hash can't do: **revocation**.

### 5.5 Why "AIR is the limitation" is structural (colleague's table, endorsed)

| AIR limitation | Why structural in AIR | Typed Shape IR |
|---|---|---|
| Silent-wrong-branch (one Rhai type-error wedged a net w/ 50k ErrorOccurred) | logic = `format!()`-interpolated Rhai checked against nothing | compile error (Strict mode) |
| int/float conflation | one `ScalarTy::Number`; JSON flattens | separate Int/Number; `as_number()` refuses i64→f64 |
| Sub-workflow duplication | child AIR embedded by value | dependency hash, shared by reference |
| No versioning | UUID + `version:i32` + git | the version *is* the content hash |

Reframe that holds: **AIR is, in its own code, an untyped IR of a distributed-
function graph; Shape is a typed IR of the same model.**

---

## 6. Replacing Rhai — the first move

### 6.1 Decision: don't build the throwaway transpiler

A "limited Phase 0" (author/check guards as Shape in mekhan, **transpile Shape →
Rhai**, engine unchanged) is throwaway **iff you commit to the engine executing
Shape**. If the engine keeps running Rhai, that transpile is load-bearing forever.
So the real content of "replace Rhai" is the decision: **the engine executes Shape,
not Rhai.** That's the correct, non-throwaway call. Skip the transpiler.

### 6.2 Rhai's two jobs split unevenly

- **Guards (pure boolean predicates): tractable, the clean first cut.** Embed
  `shape-vm` in the engine; compile guard blobs in mekhan (where producer types
  live); engine executes blob → bool. Guards are **pure → replay-trivial** (engine
  is event-sourced; run the interpreter, not the JIT; no effects to journal). Real
  work (engine dependency, eval-site surgery, blob compile) but bounded, and the
  embedded VM is **permanent** (seed of Architecture B) → nothing discarded.
- **Plumbing (`__pluck`/`__set_path`/`job_inputs`/effect configs/placeholders):
  do NOT port to Shape.** It exists because AIR is untyped and needs imperative
  token-shaping; porting it generates awkward `Json`-ADT-narrowing Shape glue that
  buys nothing. The right move is to **obviate it** — in a typed affine IR the data
  routing *is* the dataflow, and the glue evaporates. So **fully replacing Rhai
  pulls in the keystone** ("replace Rhai" ⊇ "build the IR").

### 6.3 Staging that discards nothing

1. **Shape VM in the engine executing guards as blobs.** Rhai-for-plumbing runs
   alongside temporarily (two runtimes briefly — *additive*, not throwaway).
2. **Build the Shape IR; plumbing migrates in; remaining Rhai evaporates.** The
   keystone — where end-to-end inference and AIR-from-Shape live.

### 6.4 Gating facts (replay + structuredness)

- Anything **effectful** moved from Rhai into Shape must integrate with the
  engine's **effect journaling** (`docs/12-agent-node-design.md` solved this for
  agent nodes). Guards (pure) dodge it; plumbing does not.
- Step 2 (plumbing-absorption) only works if nets are **well-structured**.

---

## 7. Open questions / next steps

Two cheap reads that turn this from a plan into scoped work:

1. **Structuredness by construction?** Read `graph.rs` + `lower/` + `wire.rs`: does
   the compiler guarantee matched, well-nested, single-entry-single-exit blocks, or
   can authors produce unstructured nets (crossing split/join, arbitrary arcs)?
   Determines whether the affine-function-graph representation is *total* (lower
   everything) or *partial* (lower the structured core; type resource-pool /
   unstructured parts as explicit `&mut` escape hatches). **Gates §4 / §5 / §6.3-2.**
2. **Engine Rhai eval site separable?** Is guard evaluation a distinct eval path or
   does it share one Rhai `Engine`/eval path with plumbing logic? **Sizes §6.2
   guards-first.**

De-risking experiment (proves the IR is faithful *and* pays): elaborate **one**
real demo — the BO map-reduce workflow (`demos/12-bo-loop`) — into Shape IR,
type-check it, lower back to AIR, and show (a) the output AIR is behaviorally
identical to today's, and (b) the type-checker **rejects the all-null collect bug
at publish**.

Strategic fork that dominates the timeline (not technical): is Shape a product you
commit to investing in independently (→ start the convergence, guards-in-Shape as
the wedge), or is the goal only better platform typing (→ build the §3 type-flow
layer natively, borrow Shape's *ideas*, no language adoption)? The user's stance in
this conversation: Shape is built-out anyway → convergence is on the table.
