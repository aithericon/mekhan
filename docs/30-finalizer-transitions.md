# 30 — Finalizer transitions: releasing held resources on permanent net failure

> Status: **implemented** (`feat/motion-planning` `811768b9`). Builds on
> [[14-resource-pool-net-design]] (claim/grant/register/release on the Petri
> substrate) and [[17-lease-scope]] (hold one allocation across a region,
> release on exit). Engine mechanism; the first (and currently only) consumer is
> the lease bridge's failure-path release.

## 1. Problem

A [[17-lease-scope]] (`LeaseScope`, or a leased `Loop`) holds exactly one
capacity unit — a presence lab runner or a datacenter allocation — across its
whole interior region, and releases it on exit. The release is wired as a single
transition `t_<id>_exit`:

```
t_<id>_exit:  consume p_<id>_body_out (the body's SUCCESS token)
            + consume p_<id>_held     (the single held-lease token)
            → emit  release { grant_id } to the pool net's release inbox
```

The `p_<id>_held` place carries **exactly one token** for the whole scope
lifetime; only `t_<id>_exit` consumes it. That single-token invariant is the
"release-exactly-once" guarantee (see [[14-resource-pool-net-design]]).

The defect: **`t_<id>_exit` is gated on body _success_** (it needs
`p_<id>_body_out`). When any interior step fails **permanently** — a Rhai
`throw`, a Decision deadend, an unhandled effect error, a pre-dispatch reject —
the engine marks the net `NetFailed` and **stops the evaluation loop**
(`evaluate_until_quiescent`'s permanent-error arm). At that point:

- `p_<id>_body_out` was never produced → `t_<id>_exit` can never fire.
- `p_<id>_held` still holds its one token → the lease is never released.
- The pool net's `in_use` keeps the unit forever.

And because the hold is **event-sourced**, restarting the engine replays the net
into the same still-held state — so a stranded runner/allocation **survives a
restart**. In practice a single failed leased run wedged a shared lab runner
until a full `just dev reset` (and, because xArm demos share one runner, blocked
every other ROS demo too).

Reaping from the supply side does not cover this. The pool net already reaps a
held unit when the **unit** dies (`presence_expired` → `t_reap_held`,
`lease_expired` → `t_reap`); there was nothing for when the **holder** (the
instance net) dies while the unit is still alive.

## 2. Design

Add a structural notion of a **finalizer transition**: a transition the engine
fires **only while tearing a permanently-failed net down**, never during normal
forward progress. The lease's failure-path release becomes one such finalizer.

A finalizer is enabled purely by its own input arcs — for the lease that is the
single held token. It must therefore be **excluded from normal selection**: its
input (the held token) is available for the entire scope lifetime, so an ordinary
selection would fire it immediately after acquire and release the lease *before
the body even runs*. The engine enforces this with a selection phase.

### 2.1 Selection phases

`select_next_transition` takes a `SelectPhase`:

- **`Normal`** — ordinary evaluation. Finalizer transitions are skipped entirely
  (gated out before the binding/memo check, so they never enter the
  negative-binding memo either).
- **`Finalizing`** — the post-failure drain. *Only* finalizer transitions are
  considered; everything else is skipped, so no ordinary work makes forward
  progress past the failure point.

### 2.2 The drain

When `evaluate_until_quiescent` decides a net has failed permanently (the
permanent-error arm, and likewise the pre-dispatch-reject arm), **before**
returning the `failure_reached` result (the driver appends `NetFailed` on top of
it), it runs `drain_finalizers`:

```
loop (bounded):
  advance marking (+ reconcile binding memo)
  pick next transition in SelectPhase::Finalizing
  none enabled → stop
  fire it through the ordinary fire path
  a finalizer that itself errors → log + stop (best-effort)
```

Each finalizer fires through the **same** `fire_transition` path as any other
transition, so:

- its effect on the marking is journaled as a normal `TransitionFired`, and
- any cross-net bridge output (the lease release) is **published to the pool
  net** as part of that firing.

Crucially this all happens **ahead of** the `NetFailed` event. So the event
order is `… ErrorOccurred, TransitionFired(t_<id>_finally), NetFailed`. On
replay the finalizer re-applies deterministically and the lease ends released —
**a restart never re-strands the unit.** The pool net, which also replays its own
events, has already consumed the release and recycled the unit.

### 2.3 The lease finalizer

The shared lease bridge (`emit_lease_bridge`, used by **both** `LeaseScope` and
the leased `Loop`) emits, alongside `t_<id>_exit`:

```
t_<id>_finally:  [finalizer]  consume p_<id>_held
                            → emit release { grant_id: held.grant_id }
```

Same release shape as `t_<id>_exit`, routed to the same plain release bridge, so
both pool backends correlate it on `grant_id` identically.

**Release-exactly-once is preserved structurally**, by the single held token:

- **Success:** `t_<id>_exit` consumes `p_<id>_held` first. The drain never runs
  (no failure), and even if it did the finalizer has no token to bind. No-op.
- **Failure:** `t_<id>_exit` never fired, so `p_<id>_held` is still full and the
  finalizer is the *only* consumer of that one token. Exactly one release.

Exactly one of `{t_exit, t_finally}` ever consumes the single held token.

## 3. Edge cases

- **Held-unit death** (the runner/alloc itself dies mid-lease): the pool net
  already reaped the hold (`t_reap_held` / `t_lease_died`) and the instance's
  `t_<id>_lease_abort` threw. The finalizer still fires and emits a release, but
  the pool has no matching `in_use` hold to correlate, so that release simply
  **orphans harmlessly** in the release inbox. The unit is not double-freed.
- **Net with no finalizers:** `drain_finalizers` selects nothing and returns
  immediately — failure handling is unchanged for non-lease nets. The drain is a
  bounded no-op cost on the (rare) failure path.
- **A finalizer that fails:** logged and the drain stops; the net still fails.
  Cleanup is best-effort, never a source of re-entrant failure.

## 4. Authoring

A finalizer is declared with the SDK builder's terminal-agnostic `.finalizer()`:

```rust
ctx.transition("t_x_finally", "Release on failure")
    .auto_input("held", &p_held)          // its only input: the held token
    .auto_output("release", &p_release_out)
    .finalizer()                          // never selected in Normal, only in the drain
    .logic_rhai("#{ release: #{ grant_id: held.grant_id } }")
    .done();
```

The flag is carried end-to-end: SDK `TransitionBuilder` → AIR
`ScenarioTransition` → api-types → `scenario_loader` → domain `Transition`
(`#[serde(default, skip_serializing_if)]`, so existing AIR round-trips
byte-identically). **Any SDK→domain shortcut must carry it too** — the
test-harness `from_sdk` converter originally dropped it, which silently demoted
the finalizer to an ordinary transition (it then won on enabling-time and
released the lease mid-run); the integration test below caught exactly that.

### Authoring rule

A finalizer must be enabled *purely by its own input arcs* (e.g. consume the
single held token). **Do not** gate it on a guard that only the success path
satisfies, and do not give it inputs that exist only transiently — its whole
purpose is to be fireable precisely when the net is wedged at failure.

## 5. Generality

Nothing about the mechanism is lease-specific. A finalizer is "run this on
permanent-failure teardown, before `NetFailed`." It is the engine-level hook for
any **cleanup/compensation on failure** — releasing a held resource, emitting a
terminal audit token, notifying a parent. The lease release is simply the first
consumer.

## 6. Verification

- **Unit (engine):** the phase gate — a finalizer is never selected in `Normal`
  even when its input is enabled, and is the only thing selected in
  `Finalizing`.
- **Unit (compiler):** the lease bridge emits `t_<id>_finally` with
  `finalizer: true`, consuming `p_<id>_held` and emitting the `grant_id`
  release; the success-path `t_<id>_exit` is **not** a finalizer.
- **Integration (engine, real service eval loop —
  `test-harness/tests/finalizer_drain.rs`):** a leased-shaped net that throws in
  its body ends `failed` *and* has released the held token exactly once via the
  finalizer; with no failure the finalizer never fires even though its input is
  continuously available.
- **Live:** a `LeaseScope` over a presence runner that deadends mid-body →
  instance `failed`, `p_<id>_held` empty, pool `in_use = 0` with the freed unit
  recorded `outcome: "released"`; a subsequent normal leased run then succeeds
  **with no reset** (the strand symptom is gone). The success path is unaffected.

## 7. Key files

| Concern | File |
|---|---|
| `finalizer` flag (domain) | `engine/core-engine/crates/domain/src/transition.rs` |
| `finalizer` flag (AIR / SDK) | `engine/sdk/src/scenario.rs`, `engine/sdk/src/transition.rs` (`.finalizer()`) |
| AIR → domain plumbing | `engine/core-engine/crates/api-types/src/lib.rs`, `api/src/scenario_bridge.rs`, `application/src/scenario_loader.rs` |
| `SelectPhase` + `drain_finalizers` | `engine/core-engine/crates/application/src/evaluation.rs` |
| `t_<id>_finally` emission | `service/src/compiler/lower/lease_bridge.rs` |
