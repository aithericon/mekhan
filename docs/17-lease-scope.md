# 17 — LeaseScope: decoupling "hold an allocation" from "loop"

> Status: design (drives the build/migrate phases). Builds on
> [[14-resource-pool-net-design]] (claim/grant/register/release on the Petri
> substrate), [[16-multi-cluster-scheduling]] (datacenter resources as
> per-flavor allocators), and the shipped `feat/slurm-lease` work (loop-scoped
> lease → persistent drain executor, `runOnLease` body enqueue).

## 1. Problem

Today "hold one cluster allocation across N body runs" is fused onto the **Loop**
node: `WorkflowNodeData::Loop { lease: Option<LeaseBinding> }`. When a Loop
carries a lease, `lower_loop` (service/src/compiler/lower/loop_.rs) hoists the
claim/grant/register/release handshake to loop scope so ONE allocation backs
every iteration and runs ONE persistent drain executor on the lease-scoped NATS
namespace (`lease-<grant_id>`). A body `AutomatedStep` with
`Scheduled { operation: Submit, run_on_lease: true }` then ENQUEUES to that
namespace instead of dispatching a fresh scheduler job
(service/src/compiler/lower/automated_step.rs).

Two things are wrong with fusing this onto Loop:

1. **You can only get a warm allocation by iterating.** A *sequential* warm
   pipeline (step A → step B → step C, all on the same held alloc, A's venv/model/
   GPU state still warm for B) is impossible — there's no loop to hang the lease
   on.
2. **`run_on_lease` is a per-step boolean flag** the author must remember to set
   on every body step, and it only means anything when the step happens to sit
   inside a leased Loop. It is redundant with containment.

## 2. Design

Introduce a **`LeaseScope` container node**. It holds exactly one datacenter
allocation for the duration of its body: **acquire on enter, release on exit**.
Any step placed inside it (`parent_id == lease_scope.id`) that targets the
cluster runs WARM on the held alloc — **implicitly, by containment**. No
per-step flag.

```
        ┌─ LeaseScope[lease=slurm_dc] ──────────────────┐
 Start ─┤  body_in → stepA → stepB → stepC → body_out   ├─ End
        └───────────────────────────────────────────────┘
```

- `Loop` goes back to meaning **just "iterate."**
- Warm iteration = a `Loop` **inside** a `LeaseScope`.
- Warm sequential pipeline = sequential steps inside a `LeaseScope` (the thing
  that's impossible today).

### 2.1 `run_on_lease` becomes implicit-by-containment

The replacement rule: **a Scheduled step enqueues to the lease namespace iff its
nearest enclosing lease-holding scope holds a lease.** "Lease-holding scope" =
a `LeaseScope` OR a `Loop { lease: Some(_) }` (the back-compat sugar, §2.3).

Concretely, the per-step `run_on_lease: bool` is removed from
`DeploymentModel::Scheduled`. The compiler computes, for a `Scheduled` step,
whether it is *lease-enclosed* by walking `parent_id` to the nearest lease-holder
(`enclosing_leased_scope_slug`, §3.2). If found, the step lowers via the EXECUTOR
enqueue path stamping `d.executor_namespace = <holder_slug>.lease.executor_namespace`;
if not, it bridges to the scheduler-net (a plain Submit) exactly as today.

This means: drop a `Scheduled` step into a `LeaseScope` → it warms automatically.
Drag it out → it becomes an independent scheduler submit. No flag to forget, no
flag to contradict the container.

### 2.2 Acquire-on-enter / release-on-exit semantics

`LeaseScope` reuses the **exact** claim/grant/register/release topology that
`lower_loop`'s leased path emits today, minus the iteration counter / continue /
exit-guard machinery:

- **enter (claim)** `t_<id>_claim`: mint `grant_id = input._instance_id + ":" +
  <id>` (replay-deterministic), emit `ClaimRequest` to the datacenter
  lease-adapter net (`pool-<resource_id>`), park `{input, grant_id}`.
- **enter (acquire)** `t_<id>_enter`: correlate `{pending, grant}` on
  `grant_id`, register the hold (plain bridge → `POOL_REGISTER_INBOX`), park the
  full lease on `p_<id>_held` for the release echo, park the lease envelope at
  `p_<id>_data` under a `lease` key (so body steps + downstream borrow
  `<scope_slug>.lease.<field>`), and hand the token to the body via
  `p_<id>_body_in`.
- **body** runs (sequential steps and/or a Loop, all children).
- **exit (release)** `t_<id>_exit`: consume the body's terminal token from
  `p_<id>_body_out` + the single `p_<id>_held` token, forward the token to
  `p_<id>_output`, arc to release_out (`POOL_RELEASE_INBOX`) keyed by
  `held.grant_id`. The single `p_held` token is the structural guarantee that
  release bridges EXACTLY ONCE (docs/14 every-terminal-releases invariant).
- **fail-fast on held-alloc death** — identical to the loop path: a `fail` reply
  channel routes held-allocation death to `p_<id>_lease_failed`, a register
  transition parks it write-once, and `t_<id>_lease_abort` consumes `p_<id>_data`
  + read-arcs the parked failure flag and `throw`s a permanent ScriptError →
  NetFailed.

The difference from Loop is purely the body-cycle: LeaseScope has NO
`t_continue`, NO iteration counter, NO `loop_condition`/`max_iterations` guard.
It enters once, runs the body once, exits once. The held lease still rides
`p_<id>_data` as a parked `lease` envelope (`Any`-typed), so the borrow surface
`<scope_slug>.lease.executor_namespace` / `<scope_slug>.lease.alloc_id` resolves
through the identical `resolves_under_opaque` path the loop lease uses
(service/src/compiler/borrow/planners/guard.rs:138).

### 2.3 Loop KEEPS its `lease` as sugar (decision)

**Decision: keep `Loop { lease }` as a back-compat sugar lowering, do NOT remove
it.** Rationale:

- The three live lease e2e (`scheduled_lease_{slurm,nomad,two_cluster}_e2e`) all
  build a `Loop { lease }` programmatically and drive it live on real Slurm/Nomad.
  Removing the Loop lease would force rewriting all three (plus the
  doc_ops round-trip test, the compiler_e2e keystone) and re-driving them live —
  high risk for a compiler-only refactor.
- A leased Loop is exactly equivalent to `LeaseScope { Loop { … } }`. Keeping the
  sugar costs nothing once the lease-bridge handshake is a **shared helper**
  (§4): `lower_loop`'s leased arm calls the helper for the
  claim/grant/register/release + parked-lease + drain-namespace plumbing, then
  layers its iteration topology on top. `lower_lease_scope` calls the same helper
  with a trivial enter→body→exit body-cycle.

So both authoring paths produce the same wire JSON / engine effects; the editor
surfaces **LeaseScope** as the recommended authoring path (a Loop's lease picker
is hidden/deprecated, §6), and `Loop { lease }` remains valid for the e2e and any
hand-built graph.

### 2.4 What does NOT change

- **No engine change.** Same `resource_lease` effect, same datacenter
  lease-adapter net (`mekhan_service::petri::pool_net`), same
  `ExecutorSubmitHandler` reading `d.executor_namespace`, same NATS namespace
  `lease-<grant_id>`, same one persistent drain executor. This is a compiler +
  editor + model change.
- The wire shape of the `ClaimRequest` / grant / register / release tokens is
  byte-identical (the shared helper emits the same Rhai).

## 3. Compiler

### 3.1 The new `lower_lease_scope`

New file `service/src/compiler/lower/lease_scope.rs`, registered like every other
lowering. Signature `fn lower_lease_scope(cx: &mut LoweringCtx) -> Result<(),
CompileError>`. It:

1. rejects an empty scope (`cx.children.is_empty()` →
   `CompileError::LeaseScopeEmpty`), mirroring `lower_loop`'s `LoopEmpty`.
2. resolves the `LeaseBinding` via `automated_step::resolve_binding(id, alias,
   request, "datacenter", cx.known_resources)` and records the lease definition /
   inbox-schema fixups (identical to loop_.rs:71-88).
3. calls the **shared lease-bridge helper** (§4) to emit claim → enter(acquire) →
   the held/failed inboxes/registers/aborts → exit(release), wiring the body in
   via `p_<id>_body_in` and out via `p_<id>_body_out`.
4. registers `NodePorts { input_place: p_input, output_places: [(None, p_output),
   (Some("body_in"), p_body_in)], input_handles: {"body_out": p_body_out} }` and
   `publish_interface().data_port = Some("p_<id>_data")` — same handle convention
   as Loop (loop_.rs:404-423).

### 3.2 Body retarget: walk parent_id for a lease-holder

`enclosing_leased_loop_slug` (automated_step.rs:1367 AND its byte-identical twin
in guard.rs:753) is generalized to **`enclosing_leased_scope_slug`**: walk
`node.parent_id` to the nearest ancestor that is EITHER a
`Loop { lease: Some(_) }` OR a `LeaseScope { … }`, returning that ancestor's
`slug()`. (It must walk the chain, not just the direct parent, because a step
can sit inside a `Loop` inside a `LeaseScope` — the `Loop` is plain, the holder
is the `LeaseScope` two levels up.)

The `Scheduled`-step dispatch in `lower_automated_step` (automated_step.rs:29-72)
no longer reads `run_on_lease`. Instead:

- `Scheduled { operation: Lease }` → `lower_automated_step_scheduled_lease`
  (unchanged).
- `Scheduled { operation: Submit }` → if `enclosing_leased_scope_slug(node,
  graph).is_some()`, fall through to the plain executor lowering (stamping
  `d.executor_namespace = <holder_slug>.lease.executor_namespace`); else
  `lower_automated_step_scheduled` (scheduler-net submit).

`ns_frag` (automated_step.rs:163-179) is computed from
`enclosing_leased_scope_slug(...)` directly (the `run_on_lease` matches! gate is
gone).

#### Implementation note (compiler phase — `run_on_lease` retained as a fallback)

The compiler phase ships LeaseScope while **keeping** `run_on_lease` honoured as a
back-compat fallback (the Migrate phase removes the flag). To keep the three live
lease e2e green *and* the leased-Loop negative control
(`scheduled_body_without_run_on_lease_does_not_borrow_alloc`) green — which
re-compiles a body inside a leased Loop with `run_on_lease: false` and asserts it
stays a scheduler-net submit — the dispatch / `ns_frag` / guard-read-arc sites all
route through ONE reconciler, `lease_namespace_holder_slug(node, graph)`
(automated_step.rs), so they cannot drift:

- a `Scheduled { Submit, run_on_lease: true }` body → `enclosing_leased_scope_slug`
  (full chain — LeaseScope **or** leased Loop): the legacy opt-in;
- any other `Scheduled { Submit }` body → `enclosing_lease_scope_container_slug`
  (LeaseScope **only**, by containment): the new implicit path. A leased Loop
  without the flag stays transparent here, preserving the pre-LeaseScope
  scheduler-net behaviour until Migrate folds the flag into pure containment.

`enclosing_leased_scope_slug` and `enclosing_lease_scope_container_slug` share a
single `parent_id`-chain walk (`enclosing_holder_slug(..., include_leased_loop)`).
The guard.rs twin helper is now a thin delegate to
`lease_namespace_holder_slug` (single source of truth).

### 3.3 Guard read-arc borrow synthesis

`guard_readarc_plan` (guard.rs:559) gets a generalized arm: for a `Scheduled {
operation: Submit }` AutomatedStep, if `enclosing_leased_scope_slug(node,
graph)` is `Some(holder_slug)`, synthesize the source
`format!("{holder_slug}.lease.executor_namespace")` (the run_on_lease match arm at
guard.rs:668-679 is replaced by this containment check). `resolve_ref`'s
`is_loop_node` branch must also accept a `LeaseScope` producer — add
`is_lease_scope_node` alongside `is_loop_node` in the
`resolve_ref` Qualified branch (guard.rs:232) so `<scope>.lease.executor_namespace`
resolves via `resolves_under_opaque` against the parked `Any` lease envelope.

`apply_guard_borrows` (apply/guard.rs) is unchanged — it already walks both
`t_<consumer>_*` and scoped `<consumer>/*` transitions and rewrites the dotted
ref to `d_<producer>.<path>`. The LeaseScope's parked place is `p_<id>_data` like
any producer, so its read-arc wires identically.

## 4. The shared lease-bridge helper

Extracted from `lower_loop`'s `Some(binding) =>` arm (loop_.rs:187-360) into a
new free fn in `service/src/compiler/lower/lease_bridge.rs` (or a `pub(super)` fn
in `automated_step.rs` next to `resolve_binding`):

```rust
/// Emit the claim/grant/register/release lease handshake at SCOPE level
/// (one allocation held across the whole body, one persistent drain executor
/// on the `lease-<grant_id>` namespace). Shared by `lower_loop`'s leased arm
/// and `lower_lease_scope`. Returns the wired interior places so the caller can
/// attach its body-cycle (Loop: enter→body, continue, exit; LeaseScope:
/// enter→body, exit).
pub(super) struct LeaseBridge {
    pub p_body_in: PlaceHandle<DynamicToken>,
    pub p_body_out: PlaceHandle<DynamicToken>,
    pub p_data: PlaceHandle<DynamicToken>,   // parked lease envelope (+ caller's extra keys)
    pub p_held: PlaceHandle<DynamicToken>,   // single held token → release-exactly-once
    pub p_release_out: PlaceHandle<DynamicToken>,
    pub p_lease_failed: PlaceHandle<DynamicToken>, // parked failure flag (read-arced)
    pub d_slug: String,                       // pre-wired parked-place binding name
}

pub(super) fn emit_lease_bridge(
    ctx: &mut Context,
    id: &str,
    label: &str,
    binding: &PoolBinding,
    // caller-supplied fragments folded into the enter/continue logic + exit:
    data_enter_extra: &str,   // Loop: ", iteration: 0<acc_enter>"; LeaseScope: ""
) -> LeaseBridge;
```

The helper owns: the `p_<id>_grant_inbox` reply channel, the
`p_<id>_lease_failed` / `p_<id>_lease_failed_parked` inbox + register, the
`p_<id>_claim_out` (grant+fail reply routing), the plain `p_<id>_register_out` /
`p_<id>_release_out` bridges, `p_<id>_pending` / `p_<id>_held`, `t_<id>_claim`,
`t_<id>_enter` (acquire, seeding `lease: grant` into the parked envelope),
`t_<id>_lease_failed_register`, and `t_<id>_lease_abort`. The grant_id expr,
claim payload, and the parked `lease: grant` seeding are all byte-for-byte what
loop_.rs emits today.

The caller adds its body-cycle:
- `lower_loop` (leased): the iteration counter is the `data_enter_extra`; it
  additionally emits `t_<id>_continue` (re-folds `lease: {slug}.lease`) and its
  held-consuming `t_<id>_exit` with the `iteration >= max || !cond` guard.
- `lower_lease_scope`: `data_enter_extra = ""`; it emits a trivial
  `t_<id>_exit` consuming `{p_body_out, p_held}` (+ a read-arc on `p_data`) →
  `{output, release}`, no guard.

The `lease_definitions` / `lease_inbox_schemas` fixups stay in the caller (they
need `cx.fixups` before the `&mut *cx.ctx` reborrow), exactly as loop_.rs does.

## 5. The LeaseScope node model

`WorkflowNodeData::LeaseScope` (service/src/models/template.rs), serde tag
`"lease_scope"`, container semantics via `parent_id` (children set
`parent_id == lease_scope.id`, attach through `body_in`/`body_out` handles like
Loop):

```rust
#[serde(rename = "lease_scope")]
LeaseScope {
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// REQUIRED datacenter lease binding (a LeaseScope with no lease is
    /// pointless — reject at validate). Reuses LeaseBinding (scheduler alias +
    /// optional claim-schema request), same `resolve_binding("datacenter")`.
    lease: LeaseBinding,
},
```

Handle convention (mirrors Loop): outer `in`/`out` perimeter handles; interior
`body_in` (source) / `body_out` (target). Parked producer: `p_<id>_data` carries
the `lease` envelope, borrowable as `<scope_slug>.lease.<field>`.

`NodeKind::LeaseScope` added to the enum (interface.rs:115) + `wire_str`
(interface.rs:166-ish), a `LEASE_SCOPE_DECL` in service/src/nodes/lease_scope.rs
+ registered in `NODES` (mod.rs:160), with `parks_data_envelope: true`,
`lower: Some(lower_lease_scope)`, `input_ports`/`output_ports` returning the
`in`+`body_out` / `out`+`body_in` shape (copy loop_.rs:43-73), a `yjs_encode`
that persists `lease` (copy loop_.rs:108-111), `validate` rejecting empty/no-lease,
and `token_shape` an `out_shape_lease_scope` (mirror `out_shape_loop`).

### 5.1 Loop KEEPS `lease` (recommended, §2.3)

`Loop { lease: Option<LeaseBinding> }` is unchanged. The editor stops offering it
(LeaseScope is the authoring path) but the field, its yjs_encode, and the leased
lowering arm all stay so the live e2e and hand-built graphs keep compiling.

## 6. Editor

- **Palette**: a `LeaseScope` container entry (driven by the new
  `LEASE_SCOPE_DECL`, surfaced through `GET /api/v1/node-types`). Icon e.g.
  Lucide `Lock`/`Container`; add to `node-palette-meta.ts` + `nodeTypes` map
  (nodes/index.ts) + `isContainer` (WorkflowCanvas.svelte:126-127:
  `t === 'lease_scope'`) so drop hit-testing reparents children + the resizer +
  the container-sort-before-children ordering all apply.
- **Node component** `nodes/LeaseScopeNode.svelte`: a near-copy of LoopNode.svelte
  — dashed resizable container with `in`/`out` perimeter handles + interior
  `body_in`/`body_out` solid handles, header reading `<label> · <dc alias>`.
- **`createDefaultNodeData('lease_scope')`** (types/editor.ts:91) → `{ type:
  'lease_scope', label: 'Lease Scope', lease: { scheduler: '' } }`; the
  `isContainer` default size `{ width: 400, height: 200 }` already applies
  (WorkflowCanvas.svelte:484-485 via `isContainer`).
- **Property panel** `LeaseScopeNodeSection.svelte` (registered in
  node-property-sections.ts + the NodePropertyPanel.svelte type gate at :288):
  a single **lease binding picker** = the datacenter ResourcePicker already used
  in DeploymentSection.svelte:227-244 (`listResources({ resource_type:
  'datacenter' })` → Select → `lease.scheduler`) + the optional raw-JSON request
  textarea (lease.request).
- **Hide/deprecate the Loop lease**: DeploymentSection already authors the
  per-step `run_on_lease`? No — `run_on_lease` is not currently surfaced in any
  panel (the e2e set it programmatically), so removing the field needs no panel
  change. The Loop lease binding is likewise not surfaced in LoopNodeSection
  today, so there is nothing to hide there either; the only editor work for the
  Loop side is NOT adding a lease picker (LeaseScope owns it). Document in the
  LoopNodeSection that warm allocation is authored by wrapping the Loop in a
  LeaseScope.

## 7. `run_on_lease` migration (remove the flag, replace by containment)

Removed entirely; each site's replacement:

| Site | Change |
|------|--------|
| `service/src/models/template.rs` `DeploymentModel::Scheduled.run_on_lease` (1297-1302) | DELETE the field (+ its doc comment 1282-1296). |
| `automated_step.rs` dispatch (42-62) | Drop the `run_on_lease: false` gate; route Submit by `enclosing_leased_scope_slug(...).is_some()` (lease-enclosed → fall through to executor enqueue; else scheduler-net). |
| `automated_step.rs` `run_on_lease`/`ns_frag` (163-179) | `ns_frag` from `enclosing_leased_scope_slug(...)` directly. |
| `automated_step.rs` `lower_automated_step_scheduled` (439, 451, 493-499) | Drop `run_on_lease` from the destructure + the debug_assert; comment update. |
| `automated_step.rs` `enclosing_leased_loop_slug` (1367) | rename+generalize → `enclosing_leased_scope_slug` (Loop-lease OR LeaseScope, walk chain). |
| `guard.rs` `Loop { lease }` arm srcs (573-615) | unchanged (Loop lease sugar stays). |
| `guard.rs` `run_on_lease: true` arm (668-679) | replace with the containment-based `enclosing_leased_scope_slug` arm. |
| `guard.rs` `enclosing_leased_loop_slug` (753) | rename+generalize, same as automated_step.rs. |
| `service/src/yjs/doc_ops.rs` test (724, 752-754) | rebuild the step without `run_on_lease`; assert the body is lease-enclosed by parentage instead (or convert the test to a LeaseScope). |
| `app/src/lib/api/schema.d.ts` `runOnLease?` (2761) | regen via `just dev::openapi` (auto). |
| `service/tests/compiler_tests.rs` (4502, 5954 `run_on_lease: false`) | drop the field. |
| `service/tests/scheduled_e2e.rs` (131), `scheduled_slurm_e2e.rs` (131) | drop `run_on_lease: false` (non-lease submits — behaviour identical). |
| `service/tests/scheduled_lease_slurm_e2e.rs` (215), `scheduled_lease_nomad_e2e.rs` (188), `scheduled_lease_two_cluster_e2e.rs` (167) | drop `run_on_lease: true` from the Scheduled step; the body is `parent_id == loop.id` and the loop carries a lease, so containment retargets it automatically. |
| `service/tests/compiler_e2e.rs` (1519-1700: keystone + negative control) | keystone: drop `run_on_lease: true`, body is lease-enclosed by the loop. Negative control `scheduled_body_without_run_on_lease_does_not_borrow_alloc` (1647): re-express as "a Scheduled body whose enclosing loop holds NO lease must not borrow" — i.e. the loop has no `lease`, so `enclosing_leased_scope_slug` is None. |
| `service/tests/fixtures/graphs/leased-loop-scheduled-body.json` (54 `runOnLease`) | remove the key; the body's `parentId` is the leased loop. |

### How the live lease e2e stay green WITHOUT the flag

The three live e2e build `Loop { lease: Some(...) }` with a child
`Scheduled { Submit }` body (`parent_id == loop.id`). With the flag gone,
`enclosing_leased_scope_slug` walks the body's parent → the leased Loop → returns
its slug → the body retargets to the executor enqueue path stamping
`d.executor_namespace`, AND `guard_readarc_plan` synthesizes the same
`<loop>.lease.executor_namespace` read-arc. **Byte-identical AIR to today** (the
flag was only ever a gate that containment now decides). So the same instance net
deploys, the same drain executor consumes `lease-<grant_id>`, and the e2e
witnesses (`body/inbox` present, no scheduler-net bridge) hold unchanged — no
re-driving on live Slurm/Nomad needed, only the test-builder edits above (drop one
bool).

## 8. Open question

**Does Loop keep `lease`?** Decided **yes** (sugar, §2.3) — default chosen to
keep the live e2e green with a one-line edit each instead of a full rewrite +
live re-drive. Revisit only if a future cleanup pass wants a single lease-holder
kind; at that point convert the three e2e to LeaseScope and delete the loop arm.
