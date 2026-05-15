# Proposal: Pre-Dispatch Hook — A General-Extensibility Extension Point for Effect Transitions

**Status:** Proposed
**Date:** 2026-05-15
**Scope:** `core-engine/crates/application/` (engine), `core-engine/crates/api/` (NetRegistry), `core-engine/crates/domain/` (events)
**Related:** ADR-15 (Lifecycle Events), ADR-17 (Artifact Provenance), ADR-18 (Event-Log Causality)

> Cross-repo cross-reference: the cloud-layer wave plan at
> `online-clinic/plan/cloud-layer-phase-2.md` § 2.2 specifies a consumer-side
> impl (`CapabilityRoutingHook`) that registers against this extension point.
> This proposal is consumer-agnostic — the cloud-layer impl lives in that
> repo, not here.

## 1. Goal + non-goals

**Goal.** Introduce a general-purpose extension point on `NetRegistry` that lets external consumers observe — and conditionally modify, reject, or defer — the dispatch of an effect transition *before* the registered `EffectHandler::execute` is called. The motivating use case is capability-aware compute-pool routing (a cloud-layer consumer that decides which pool a job should land on by consulting an out-of-process routing service), but the hook is GENERAL: telemetry, policy enforcement, fairness scheduling, tenant quotas, and chaos-engineering injectors are all valid consumers.

**Non-goals.** This proposal does NOT bake cloud-layer concerns, capability schemas, model-registry shapes, or any specific consumer's vocabulary into petri-lab. The trait is consumer-agnostic; specific impls live in consumer repos. The hook is also NOT a replacement for `EffectHandler` — handlers still execute side effects; the hook only sits at the dispatch boundary that precedes them. Replay mode is unaffected; hooks fire only in `ExecutionMode::Live`.

## 2. `PreDispatchHook` trait signature

The trait mirrors existing petri-lab conventions (`EffectHandler`, `TimerClient`): `async_trait`, `Send + Sync`, single primary method, human-readable `name()`. It lives in a new module `core-engine/crates/application/src/pre_dispatch.rs`.

```rust
#[async_trait::async_trait]
pub trait PreDispatchHook: Send + Sync {
    /// Called immediately before `EffectHandler::execute` for a firing
    /// effect transition. The hook MUST NOT perform side effects that
    /// would be re-applied on replay; replay mode skips hooks entirely.
    async fn pre_dispatch(
        &self,
        ctx: &PreDispatchContext<'_>,
    ) -> Result<PreDispatchOutcome, PreDispatchError>;

    /// Human-readable hook name for logging and event records.
    fn name(&self) -> &str;
}
```

The `'_` lifetime on `PreDispatchContext<'_>` reflects that the context borrows from the firing-loop's local state (binding, transition definition); the hook holds the borrow only for the duration of its `pre_dispatch` call.

## 3. `PreDispatchContext` shape

Read-mostly view of the dispatch attempt. Hooks DO NOT mutate the context in place; they return modifications via `PreDispatchOutcome::Continue`.

```rust
pub struct PreDispatchContext<'a> {
    /// Net the transition belongs to (e.g. `"clinic-letter-gen-7af3"`).
    pub net_id: &'a str,

    /// The transition about to fire.
    pub transition_id: &'a TransitionId,
    pub transition_name: &'a str,
    /// Optional `effect_handler_id` declared on the transition.
    pub effect_handler_id: Option<&'a str>,

    /// Bound input tokens (port name → JSON), as `EffectInput::inputs`.
    pub inputs: &'a HashMap<String, serde_json::Value>,
    /// Read-arc tokens (port name → JSON), as `EffectInput::read_inputs`.
    pub read_inputs: &'a HashMap<String, serde_json::Value>,

    /// Resolved effect config from the transition definition (secrets
    /// already resolved by the firing pipeline).
    pub effect_config: Option<&'a serde_json::Value>,

    /// Net-level parameters from `CreateNetRequest.parameters`.
    pub net_parameters: Option<&'a serde_json::Value>,

    /// Per-firing metadata (correlation_id, tenant_id, process_step,
    /// scenario_id, hook-chain index). Owned strings on construction so
    /// hooks may clone for outbound HTTP without borrowing the engine.
    pub metadata: PreDispatchMetadata,
}

#[derive(Clone, Debug)]
pub struct PreDispatchMetadata {
    pub scenario_id: Option<String>,
    pub tenant_id: Option<String>,
    pub correlation_id: Option<String>,
    pub process_step: Option<String>,
    /// Zero-based position of this hook in the registered chain.
    pub hook_chain_index: u32,
}
```

The context deliberately excludes `effect_handler_id`'s registered `Arc<dyn EffectHandler>` — the hook sees only the *declaration* of intent to dispatch, not the handler instance itself. Hooks remain stateless w.r.t. handler internals.

## 4. `PreDispatchOutcome` shape

The hook returns one of three variants. Petri-net semantics for each are spelled out explicitly because they affect token flow.

```rust
pub enum PreDispatchOutcome {
    /// Proceed with dispatch. If `enriched_effect_config` is `Some`, the
    /// engine replaces `EffectInput.config` with the enriched value
    /// before calling `EffectHandler::execute`. Enrichment is the only
    /// permitted mutation; inputs/read_inputs are NOT modifiable.
    Continue {
        enriched_effect_config: Option<serde_json::Value>,
    },

    /// Abort dispatch. The transition does NOT fire: no tokens are
    /// consumed, no tokens are produced, no `EffectCompleted` event is
    /// emitted. A `PreDispatchRejected` event IS emitted (see § 9). The
    /// transition becomes eligible to fire again on the next eval pass
    /// — Reject is idempotent w.r.t. marking, so retry without state
    /// loss is the expected behaviour.
    Reject {
        /// Human-readable reason for the audit log.
        reason: String,
    },

    /// Defer dispatch. Same marking impact as `Reject` (no consumption,
    /// no production), but the engine schedules a retry after
    /// `retry_after`. A `PreDispatchDeferred` event IS emitted. Deferral
    /// counts against a per-transition `max_defers` budget (configurable,
    /// default 10) — exceeding it escalates to `Reject` with reason
    /// `"defer-budget-exceeded"`.
    Defer {
        retry_after: std::time::Duration,
    },
}
```

**Net semantics summary.** `Continue` is the only outcome that consumes tokens. `Reject` and `Defer` are non-destructive — the marking is unchanged when the hook returns. This matches the existing semantics of `GuardNotSatisfied` (see `firing.rs` § `fire_effect_transition`): a transition whose preconditions fail at the dispatch boundary leaves its inputs in place for re-evaluation.

## 5. TOML registration shape

Hooks are declared at the top-level petri-lab service config (i.e. the file that drives `NetRegistry` construction), NOT per-scenario. Per-scenario hook overrides are explicitly out of scope for the first cut; if a scenario needs to opt out, it does so by tagging transitions with a label the hook itself checks.

Two transport variants, distinguished by `transport`:

```toml
# Builtin variant — Rust-impl registered via NetRegistry API (§ 6).
[[pre_dispatch_hooks]]
name = "tenant-quota-enforcer"
transport = "builtin"
# Builtin hooks are matched by `name` against the registry's
# `register_pre_dispatch_hook` entries. No additional config fields.
fail_open = false
timeout_ms = 200

# HTTP variant — out-of-process, no Rust linkage required.
[[pre_dispatch_hooks]]
name = "capability-routing"
transport = "http"
url = "http://cloud-layer-capability-routing.internal:7000/v1/routes/pick"
timeout_ms = 500
fail_open = false
# Optional: restrict the hook to specific effect_handler_ids. Empty/absent
# means "fire for every effect transition".
match_effect_handlers = ["executor_submit"]
# Optional: max retries for HTTP-level failures (connect/read errors).
# Application-level errors (5xx with valid JSON outcome) are NOT retried.
http_max_retries = 2
```

Hooks execute in declaration order. The first hook that returns `Reject` or `Defer` short-circuits the chain.

## 6. `NetRegistry::register_pre_dispatch_hook` API surface

Builtin hooks register via the registry, mirroring the existing
`register_effect_handler` pattern observed in `application/src/service.rs`. Registration MUST happen before `NetRegistry::create_or_load_net` is called for any net — registering after a net is hot results in `RegistrationError::RegistryFrozen`.

```rust
impl<E, T, S> NetRegistry<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    /// Register a builtin pre-dispatch hook under the given name.
    ///
    /// The `name` must match a `[[pre_dispatch_hooks]] name = "<name>"`
    /// entry with `transport = "builtin"` in the loaded service config.
    /// Hooks for which no matching config entry exists are rejected at
    /// registration time (fail-fast: misconfiguration is a startup bug,
    /// not a runtime degradation).
    pub fn register_pre_dispatch_hook(
        &self,
        name: impl Into<String>,
        hook: Arc<dyn PreDispatchHook>,
    ) -> Result<(), RegistrationError>;
}
```

HTTP-transport hooks are NOT registered via this API — they are instantiated by the engine itself from the TOML config (the engine ships a builtin `HttpPreDispatchHook` impl that wraps any URL).

**Chain composition.** The engine assembles the firing-time hook chain by reading the TOML config in declaration order, resolving each entry against (a) the registered builtin map and (b) the engine's HTTP-transport factory. The resolved chain is immutable per `NetInstance` once a net goes hot.

## 7. HTTP-transport wire format

Out-of-process hooks (the cloud-layer's intended path) speak a JSON-over-HTTP protocol. The schemas are stable; consumer impls can be in any language.

**Request:** `POST <url>` with `Content-Type: application/json`.

```json
{
  "net_id": "clinic-letter-gen-7af3",
  "transition_id": "submit-job",
  "transition_name": "Submit Job",
  "effect_handler_id": "executor_submit",
  "inputs": { "job": { "...": "..." } },
  "read_inputs": {},
  "effect_config": { "queue": "default", "...": "..." },
  "net_parameters": { "patient_id": "...redacted..." },
  "metadata": {
    "scenario_id": "letter-gen-v3",
    "tenant_id": "tenant-7af3",
    "correlation_id": "corr-2026-05-15-001",
    "process_step": "generate-letter",
    "hook_chain_index": 0
  }
}
```

**Response:** `200 OK` with `Content-Type: application/json`. The `outcome` discriminator MUST be one of `"continue"`, `"reject"`, `"defer"`.

```json
{ "outcome": "continue", "enriched_effect_config": { "queue": "gpu-h100" } }
```
```json
{ "outcome": "reject", "reason": "no capability for sae-tier-2" }
```
```json
{ "outcome": "defer", "retry_after_ms": 1500 }
```

**Non-2xx responses, malformed JSON, or unknown `outcome` discriminator** are treated as hook errors and routed through the `fail_open` flag (§ 8). Network-level failures (connect, DNS, TCP reset) trigger up to `http_max_retries` retries with exponential backoff (50ms × 2^n, capped at `timeout_ms`); application-level errors are NOT retried.

**Stability commitment.** This wire format and the Rust `PreDispatchOutcome` enum share the same serde-derived JSON layout (the Rust types use `#[serde(tag = "outcome", rename_all = "snake_case")]`). Adding new outcome variants is a breaking change to both surfaces and requires a versioned migration.

## 8. Error handling — fail-closed default, fail-open opt-in

**Default: fail-closed.** Any hook error — handler panic (caught and reported), HTTP non-2xx, malformed response, timeout — causes the dispatch to abort identically to `Reject { reason: "<hook-name>: hook-error: <message>" }`. The transition does NOT fire. The audit trail records why.

**Opt-in: `fail_open = true`.** For non-critical hooks (telemetry, audit-only emitters), the TOML config may set `fail_open = true`. In that mode, hook errors are logged and the chain continues to the next hook — the failing hook is treated as if it had returned `Continue { enriched_effect_config: None }`. Telemetry-only hooks SHOULD set `fail_open = true`; capability-routing / policy-enforcement hooks MUST NOT.

**Timeouts.** Per-hook `timeout_ms` (default 500ms) applies to the full `pre_dispatch` call. The engine wraps each invocation in `tokio::time::timeout`. Timeout = error = fail-closed (or fail-open per config).

**Panic safety.** Builtin hooks that panic are caught via `std::panic::catch_unwind` at the registration-boundary wrapper; the panic is converted to `PreDispatchError::HookPanicked(name)` and handled per `fail_open`.

```rust
#[derive(Debug, Clone, thiserror::Error)]
pub enum PreDispatchError {
    #[error("Hook '{0}' timed out after {1:?}")]
    Timeout(String, std::time::Duration),

    #[error("Hook '{0}' returned malformed response: {1}")]
    MalformedResponse(String, String),

    #[error("Hook '{0}' transport error: {1}")]
    Transport(String, String),

    #[error("Hook '{0}' panicked")]
    HookPanicked(String),

    #[error("Hook '{0}' execution failed: {1}")]
    ExecutionFailed(String, String),
}
```

## 9. Event-log integration

Hook decisions are first-class entries in the petri-lab event log so the audit trail captures them alongside `TransitionFired` / `EffectCompleted`. Three new `DomainEvent` variants in `core-engine/crates/domain/src/events.rs`:

```rust
DomainEvent::PreDispatchEvaluated {
    transition_id: TransitionId,
    transition_name: Option<String>,
    hook_chain: Vec<PreDispatchHookOutcome>, // one entry per hook fired
    final_outcome: PreDispatchOutcomeKind,    // continue | reject | defer
    timestamp: chrono::DateTime<chrono::Utc>,
}

DomainEvent::PreDispatchRejected {
    transition_id: TransitionId,
    hook_name: String,
    reason: String,
    timestamp: chrono::DateTime<chrono::Utc>,
}

DomainEvent::PreDispatchDeferred {
    transition_id: TransitionId,
    hook_name: String,
    retry_after_ms: u64,
    defer_count: u32, // how many times this transition has been deferred
    timestamp: chrono::DateTime<chrono::Utc>,
}
```

`PreDispatchEvaluated` is always emitted (one per dispatch attempt, regardless of outcome). `PreDispatchRejected` and `PreDispatchDeferred` are emitted on those outcomes IN ADDITION to `PreDispatchEvaluated`, so downstream consumers that only care about the terminal-rejection signal can subscribe to a narrower subject.

**Causality (ADR-18).** Rejected/deferred dispatches do NOT have consumed/produced tokens (the marking didn't change), so they DO NOT appear in `causality_event_tokens`. They DO appear in `causality_events` with `event_type = 'PreDispatchRejected'` etc., which lets Mekhan-side ancestry walks surface the audit record when investigating why a downstream artifact never materialised. Hash-chain integrity is preserved as for any other event.

## 10. Cert plan

Per `feedback_act2_certification_is_tier_scoped` — each tier named explicitly with the literal recipe and the bin/test that exercises it.

**Tier 1 — compile + lint + fmt** (per justfile `check`, `fmt`, `lint` recipes):
- `just check` — `cargo check --workspace`, must build the new `pre_dispatch` module and the new `DomainEvent` variants clean.
- `just fmt` — `cargo fmt --all -- --check`, must pass without diff.
- `just lint` — `cargo clippy --workspace --all-targets -- -D warnings`, no new warnings.

**Tier 2 — unit tests** (per justfile `test` recipe):
- `just test` — `cargo test --workspace`. Adds the following new test modules:
    - `pre_dispatch::tests::test_continue_passes_through` — registers a stub hook returning `Continue { enriched_effect_config: None }`; asserts the dispatched `EffectInput.config` is unchanged.
    - `pre_dispatch::tests::test_continue_enriches_config` — stub hook returns `Continue { enriched_effect_config: Some(...) }`; asserts `EffectHandler::execute` saw the enriched config.
    - `pre_dispatch::tests::test_reject_blocks_firing` — stub hook returns `Reject`; asserts no `EffectCompleted` event was appended, the marking is unchanged, and a `PreDispatchRejected` event WAS appended (honest-absence + honest-presence).
    - `pre_dispatch::tests::test_defer_blocks_firing_and_emits_event` — stub returns `Defer`; asserts marking unchanged, `PreDispatchDeferred` emitted with correct `retry_after_ms`.
    - `pre_dispatch::tests::test_defer_budget_exhaustion_escalates_to_reject` — stub returns `Defer` 11 times; asserts the 11th attempt produces `PreDispatchRejected { reason: "defer-budget-exceeded" }`.
    - `pre_dispatch::tests::test_chain_short_circuits_on_first_reject` — register two hooks; first returns `Reject`; assert second hook's `pre_dispatch` was never called.
    - `pre_dispatch::tests::test_fail_closed_on_hook_error_default` — hook returns `Err(...)`; assert behaviour identical to `Reject`.
    - `pre_dispatch::tests::test_fail_open_on_hook_error_when_configured` — `fail_open = true`; hook returns `Err(...)`; assert dispatch proceeds.

**Tier 3 — integration tests with NATS** (per justfile `test-integration` recipe):
- `just test-integration` — runs `test-nats-rust` (in-process Rust tests in `petri-nats` crate against a real NATS) + the shell-based `_integration-test`. Adds a new file `core-engine/crates/nats/tests/pre_dispatch_integration.rs` that:
    - **Resolve-or-seed**: brings up a fresh NATS via the existing `just infra nats-up` precondition; resolves the scenario by stable name `pre_dispatch_smoke`.
    - **Reject path**: registers a synthetic hook that rejects the `submit_job` transition; fires the scenario; asserts the transition's input tokens are still present in `MarkingProjection`, no executor JobSpec was published to `petri.executor.submit.*` (honest-absence), and a `PreDispatchRejected` event is in the JetStream event log.
    - **Continue+enrich path**: re-runs the scenario with a hook that returns `Continue { enriched_effect_config: Some({"queue": "alt"}) }`; asserts the `EffectCompleted` event records the enriched config in `effect_result`, and the executor JobSpec on NATS carries `queue: "alt"`.
    - **HTTP-transport path**: spins up a tiny in-test `hyper` server that returns canned JSON outcomes; asserts the engine consumes the HTTP outcome correctly under both `continue` and `reject`.
    - No global-state mutation — each test owns its own `NetRegistry` instance, its own JetStream subject prefix, and tears down on drop.

There is no "end-to-end stack-up" tier defined here because petri-lab's `test-integration` already integrates NATS; the cloud-layer-side stack-up cert lives in the consumer's plan doc (A3).

## 11. Trip-wires likely to surface during B1 implementation

These are flagged up-front so the implementer can plan for them, not discovered mid-impl.

1. **`async_trait` object-safety with `Box<dyn Future>` futures.** The trait method must be object-safe so the registry can store `Arc<dyn PreDispatchHook>`. `async_trait` 0.1.x generates the appropriate boxed-future signature, but the lifetime `'_` on `PreDispatchContext<'_>` interacts with the macro's elision rules — the impl may need an explicit `for<'a>` higher-rank bound on the registry-side storage type. Verify against the existing `EffectHandler` precedent (which avoids the issue by passing owned `EffectInput`).

2. **Wire-format ↔ Rust-enum drift.** Section 7 commits to `#[serde(tag = "outcome", rename_all = "snake_case")]` on `PreDispatchOutcome`. If the implementer chooses an externally tagged or adjacently tagged representation instead, the HTTP wire format diverges from the Rust trait surface — the spec's stability commitment is broken. The implementer must verify with a roundtrip serde test (`PreDispatchOutcome` → JSON → `PreDispatchOutcome`) that produces byte-identical output to the § 7 examples.

3. **Hook chain ordering: chain vs first-match.** This proposal commits to *chain in declaration order, first short-circuiting outcome wins*. An alternative — "evaluate all hooks in parallel, combine outcomes" — was considered and rejected because it makes `enriched_effect_config` merging ambiguous (whose enrichment wins?). The implementer should not silently flip to parallel evaluation; if a future use case needs it, it deserves its own proposal.

4. **`Defer` retry without resolution → unbounded retry storm.** The defer-budget escalation (§ 4: default `max_defers = 10`) is the bound. Implementer should verify the budget is enforced at the per-(net_id, transition_id) granularity, not globally — otherwise a noisy transition starves the budget for unrelated ones. Budget counters live in-memory on the `NetInstance` and are NOT persisted across hibernation (waking a hibernated net resets the counter, which is acceptable: a long-hibernated transition is presumed to have lost the resource-pressure context that caused the original defers).

5. **Hook fires during net rehydration / replay.** Replay mode MUST NOT call hooks — replay reads stored `EffectCompleted` events from the log; the hook chain's prior decisions are implicit in what was logged. The implementer must gate the hook chain on `*execution_mode.read() == ExecutionMode::Live`, mirroring how `fire_effect_transition` already gates `handler.execute`.

6. **Tenant isolation in HTTP-transport hooks.** The HTTP wire format passes `tenant_id` in metadata but does NOT enforce that the remote endpoint scopes its decision to that tenant. Consumers (e.g. cloud-layer) must enforce tenancy on the remote side; petri-lab is not in a position to. This is documented as a consumer-responsibility, not an engine guarantee.

7. **Hook chain registration AFTER nets are hot.** The fail-fast `RegistrationError::RegistryFrozen` (§ 6) prevents this, but it requires the registry to track a `frozen: AtomicBool` flag flipped on the first net-create. The implementer must ensure the flag is set transactionally so a concurrent register + create race doesn't slip through.

## Consequences

**Positive.**
- One general extension point replaces the alternative of N consumer-specific hardcoded integrations (capability routing, quota enforcement, telemetry, chaos, …) each cutting their own seam through `firing.rs`.
- Cloud-layer's capability-routing impl (A3) is HTTP-transport, so no cross-repo Rust linkage is introduced. Petri-lab remains general-purpose.
- Audit trail gains a first-class record of dispatch-time policy decisions; ADR-18's causality graph picks them up automatically.
- Fail-closed default is consistent with petri-lab's existing safety posture (`EffectError::Fatal`, `GuardNotSatisfied`).

**Negative.**
- Three new `DomainEvent` variants on a hash-chained log → all event-log readers (Mekhan included) must learn to deserialize them or gracefully ignore via `#[serde(other)]`.
- HTTP transport on the dispatch path adds latency (default 500ms timeout budget). Consumers that don't need a hook pay only the no-op cost (empty chain = zero hooks fired) but the boundary check itself is unconditional.
- `Defer` introduces a retry primitive previously absent at the dispatch boundary. Misuse (a buggy hook that defers forever) is bounded by the budget but still wasteful — operators should monitor `PreDispatchDeferred` event rates.

## Open questions (deferred to B1)

- Should the hook see the *transition guard expression* (Rhai source) or just the *resolved* binding? This proposal exposes only the resolved binding (port inputs). A future hook that wants to make a policy decision based on guard structure (e.g. "this transition uses a guard that references PHI fields, route to a clean-room pool") would need guard introspection added to `PreDispatchContext`. Deferred.
- Should `Continue` allow modifying `inputs` (not just `effect_config`)? This proposal forbids it because token-mutating hooks break the causality model (the token a hook produces has no recorded origin). If a future use case needs it, it must define provenance semantics first.
- Per-scenario hook overrides — currently out of scope (§ 5). If demand surfaces, the natural extension is a per-scenario `[[pre_dispatch_hooks]]` block that supplements (not replaces) the global chain.
