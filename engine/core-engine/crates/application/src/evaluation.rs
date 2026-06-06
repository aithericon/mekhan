use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use petri_domain::{Marking, PersistedEvent, PlaceKind, TokenColor, TransitionId};

use aithericon_secrets::SecretStore;

use crate::binding::find_valid_binding;
use crate::effect::{EffectHandler, ExecutionMode};
use crate::firing::fire_transition;
use crate::pre_dispatch::PreDispatchRuntime;
use crate::schema_registry::SchemaRegistry;
use crate::{
    EventRepository, ServiceError, StateProjection, TopologyRepository, TransitionExecutor,
};

/// Info about a terminal place that was reached.
#[derive(Clone, Debug)]
pub struct TerminalReachedInfo {
    /// The terminal place that has a token.
    pub place_id: String,
    /// Exit code extracted from the terminal token's data (if present).
    pub exit_code: Option<serde_json::Value>,
}

/// Info about a permanent firing failure that stopped the eval pass.
///
/// Set when a transition failed with a permanent error
/// ([`ServiceError::is_permanent`]). The firing layer has already advanced
/// the marking (consumed the offending tokens) and emitted the audit event;
/// this signals the eval-loop driver to raise a net-level `NetFailed` marker
/// and tear the net down rather than spin.
#[derive(Clone, Debug)]
pub struct FailureInfo {
    /// The transition whose firing failed permanently.
    pub transition_id: TransitionId,
    /// Human-readable failure reason (the `ServiceError` display string).
    pub reason: String,
    /// Whether the underlying error was classified retryable (audit only —
    /// the net fails regardless; retry is authored via an `_error` port).
    pub retryable: bool,
}

/// Result of evaluating transitions until quiescence.
#[derive(Clone, Debug)]
pub struct EvaluateResult {
    /// Number of transitions fired
    pub steps_executed: usize,
    /// IDs and sequence numbers of fired transitions
    pub transitions_fired: Vec<(TransitionId, u64)>,
    /// Final state: quiescent or limit reached
    pub final_state: EvaluateFinalState,
    /// Events generated during evaluation (for adapter notification)
    pub events: Vec<PersistedEvent>,
    /// If quiescent and a terminal place has tokens, contains the terminal info.
    pub terminal_reached: Option<TerminalReachedInfo>,
    /// If the pass stopped because a transition failed permanently, the details.
    /// The marking was already advanced by the firing layer; the driver should
    /// emit `NetFailed` and stop the net.
    pub failure_reached: Option<FailureInfo>,
}

/// Why evaluation stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvaluateFinalState {
    /// No more transitions can fire
    Quiescent,
    /// Max steps limit was reached
    LimitReached,
}

/// Detailed status of why a transition is enabled or disabled.
#[derive(Clone, Debug)]
pub enum TransitionStatusDetail {
    Enabled,
    DisabledNoTokens { missing_place: String },
    DisabledGuardFailed { guard: String },
    DisabledGuardError { error: String },
}

/// Check if a transition is enabled (can fire).
pub(crate) fn is_enabled(
    executor: &TransitionExecutor,
    topology: &impl TopologyRepository,
    transition_id: &TransitionId,
    marking: &Marking,
    schema_registry: Option<&SchemaRegistry>,
) -> Result<bool, ServiceError> {
    let net = topology.get_topology().ok_or(ServiceError::NoTopology)?;

    let transition = net
        .get_transition(transition_id)
        .ok_or_else(|| ServiceError::TransitionNotFound(transition_id.clone()))?
        .clone();

    let input_arcs = net.input_arcs(transition_id);

    Ok(find_valid_binding(executor, &transition, &input_arcs, marking, schema_registry).is_some())
}

/// Get all enabled transitions.
pub(crate) fn enabled_transitions(
    executor: &TransitionExecutor,
    topology: &impl TopologyRepository,
    marking: &Marking,
    schema_registry: Option<&SchemaRegistry>,
) -> Result<Vec<TransitionId>, ServiceError> {
    let net = topology.get_topology().ok_or(ServiceError::NoTopology)?;

    let mut enabled = Vec::new();
    for transition in net.transitions.values() {
        let input_arcs = net.input_arcs(&transition.id);
        if find_valid_binding(executor, transition, &input_arcs, marking, schema_registry).is_some()
        {
            enabled.push(transition.id.clone());
        }
    }

    Ok(enabled)
}

/// Get the status of all transitions with reasons for being disabled.
pub(crate) fn transition_statuses(
    executor: &TransitionExecutor,
    topology: &impl TopologyRepository,
    marking: &Marking,
    schema_registry: Option<&SchemaRegistry>,
) -> Result<HashMap<TransitionId, TransitionStatusDetail>, ServiceError> {
    let net = topology.get_topology().ok_or(ServiceError::NoTopology)?;

    let mut statuses = HashMap::new();

    for transition in net.transitions.values() {
        let transition = transition.clone();
        let input_arcs = net.input_arcs(&transition.id);

        // First check if all input places have enough tokens
        let mut missing_place = None;
        for arc in &input_arcs {
            let tokens = marking.tokens_at(&arc.place_id);
            if tokens.len() < arc.weight {
                missing_place = Some(arc.place_id.to_string());
                break;
            }
        }

        if let Some(place) = missing_place {
            statuses.insert(
                transition.id.clone(),
                TransitionStatusDetail::DisabledNoTokens {
                    missing_place: place,
                },
            );
            continue;
        }

        // Use find_valid_binding to search all token combinations
        match find_valid_binding(executor, &transition, &input_arcs, marking, schema_registry) {
            Some(_) => {
                statuses.insert(transition.id.clone(), TransitionStatusDetail::Enabled);
            }
            None => {
                // No valid binding found - guard must have failed for all combinations
                if let Some(guard_script) = &transition.guard {
                    statuses.insert(
                        transition.id.clone(),
                        TransitionStatusDetail::DisabledGuardFailed {
                            guard: guard_script.clone(),
                        },
                    );
                } else {
                    // No guard but still no binding - shouldn't happen if tokens exist
                    statuses.insert(transition.id.clone(), TransitionStatusDetail::Enabled);
                }
            }
        }
    }

    Ok(statuses)
}

/// Compute the enabling time for a transition.
pub(crate) fn transition_enabling_time(
    executor: &TransitionExecutor,
    topology: &impl TopologyRepository,
    transition_id: &TransitionId,
    marking: &Marking,
    schema_registry: Option<&SchemaRegistry>,
) -> Result<Option<DateTime<Utc>>, ServiceError> {
    let net = topology.get_topology().ok_or(ServiceError::NoTopology)?;

    let transition = net
        .get_transition(transition_id)
        .ok_or_else(|| ServiceError::TransitionNotFound(transition_id.clone()))?
        .clone();

    let input_arcs = net.input_arcs(transition_id);

    match find_valid_binding(executor, &transition, &input_arcs, marking, schema_registry) {
        Some(binding) => Ok(binding.max_created_at),
        None => Ok(None),
    }
}

/// Which class of transitions [`select_next_transition`] considers — the gate
/// that keeps finalizers out of normal evaluation and restricts the
/// post-failure drain to finalizers only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SelectPhase {
    /// Normal evaluation: finalizer transitions are NEVER selected (a finalizer
    /// is enabled by its own input arcs — e.g. the held lease token — for the
    /// whole scope lifetime, so selecting it normally would steal the lease
    /// before the body runs).
    Normal,
    /// Post-failure drain: ONLY finalizer transitions are selected, so a net
    /// about to be torn down releases the resources it still holds (and nothing
    /// else makes forward progress past the failure point).
    Finalizing,
}

/// Select the next transition to fire based on enabling time, specificity, and token priority.
///
/// Priority order:
/// 1. Earliest enabling time (oldest tokens)
/// 2. More specific transitions (more input arcs)
/// 3. Higher token priority (from priority expression)
/// 4. Transition ID (alphabetical tiebreaker)
///
/// `phase` gates which transitions are eligible at all (see [`SelectPhase`]).
pub(crate) fn select_next_transition(
    executor: &TransitionExecutor,
    topology: &impl TopologyRepository,
    marking: &Marking,
    schema_registry: Option<&SchemaRegistry>,
    memo: Option<&RwLock<crate::binding_memo::BindingMemo>>,
    phase: SelectPhase,
) -> Result<Option<TransitionId>, ServiceError> {
    let net = topology.get_topology().ok_or(ServiceError::NoTopology)?;

    let mut best_id: Option<TransitionId> = None;
    let mut best_time: Option<DateTime<Utc>> = None;
    let mut best_input_count: usize = 0;
    let mut best_priority: Option<f64> = None;

    // Negative-binding memo: a one-shot snapshot of transitions already proven
    // to have no valid binding at this marking (reconciled from the same event
    // delta that produced `marking`, so it never lags it). Skipping them is
    // selection-equivalent — they would each return `None` anyway. Newly-proven
    // empties are buffered and written back once at the end.
    let known_empty = memo.map(|m| m.read().unwrap().snapshot());
    let mut newly_empty: Vec<TransitionId> = Vec::new();

    for transition in net.transitions.values() {
        // Phase gate (BEFORE the memo + binding check, so finalizers never
        // enter the negative-binding memo): a finalizer is eligible ONLY in the
        // Finalizing drain; everything else ONLY in Normal evaluation.
        match phase {
            SelectPhase::Normal if transition.finalizer => continue,
            SelectPhase::Finalizing if !transition.finalizer => continue,
            _ => {}
        }

        if let Some(known) = &known_empty {
            if known.contains(&transition.id) {
                continue;
            }
        }

        let input_arcs = net.input_arcs(&transition.id);

        // Find a valid binding for this transition
        let binding =
            find_valid_binding(executor, transition, &input_arcs, marking, schema_registry);
        if binding.is_none() && memo.is_some() {
            newly_empty.push(transition.id.clone());
        }
        if let Some(binding) = binding {
            let enabling_time = binding.max_created_at;

            // Count input arcs for this transition (specificity)
            let input_count = input_arcs.len();

            // Evaluate priority expression if present
            let priority_score = transition.priority.as_ref().and_then(|priority_expr| {
                executor.evaluate_priority(priority_expr, &binding.port_inputs)
            });

            let is_better = match (&best_id, &best_time) {
                (Some(current_best_id), Some(current_best_time)) => {
                    match enabling_time {
                        Some(time) if time < *current_best_time => {
                            // 1. Earlier time always wins
                            true
                        }
                        Some(time) if time == *current_best_time => {
                            if input_count > best_input_count {
                                // 2. Same time - prefer more inputs (more specific)
                                true
                            } else if input_count == best_input_count {
                                // 3. Same inputs - compare token priority
                                match (priority_score, best_priority) {
                                    // Strictly higher priority wins outright.
                                    (Some(p), Some(bp)) if p > bp => true,
                                    // Strictly lower priority loses outright —
                                    // do NOT fall through to alphabetical id
                                    // tiebreak (which would otherwise let the
                                    // Decision `t_dec_deadend` (priority 0) beat
                                    // `t_dec_default` (priority 1) because the
                                    // `_` arm runs id-cmp regardless of who is
                                    // higher). Bug: caught by service-level
                                    // decision_e2e default-branch routing.
                                    (Some(p), Some(bp)) if p < bp => false,
                                    (Some(_), None) => true,
                                    (None, Some(_)) => false,
                                    _ => {
                                        // 4. Same priority - use ID as tiebreaker
                                        transition.id.to_string() < current_best_id.to_string()
                                    }
                                }
                            } else {
                                false
                            }
                        }
                        _ => false,
                    }
                }
                _ => enabling_time.is_some(),
            };

            if is_better {
                best_id = Some(transition.id.clone());
                best_time = enabling_time;
                best_input_count = input_count;
                best_priority = priority_score;
            }
        }
    }

    if let Some(m) = memo {
        if !newly_empty.is_empty() {
            let mut guard = m.write().unwrap();
            for t in newly_empty {
                guard.mark_empty(t);
            }
        }
    }

    Ok(best_id)
}

/// Bring the negative-binding memo up to date with the marking cache, using the
/// delta reported by [`advance_marking`]. The topology is consulted (cloned)
/// only when the reverse index must be (re)built — never on the common
/// token-event path — so the hot loop pays nothing extra for unaffected ticks.
///
/// MUST be called on **every** `advance_marking` (including read-path marking
/// reads), because any caller that absorbs events into the marking cache also
/// advances its cursor; a later eval would then see a cache hit and skip
/// reconciliation, leaving the memo stale. Binding it to the advance keeps the
/// memo and the marking cache moving from the exact same event delta.
pub(crate) fn reconcile_binding_memo<T: TopologyRepository>(
    memo: &RwLock<crate::binding_memo::BindingMemo>,
    topology: &T,
    delta: &MarkingDelta,
) {
    // Fast path for the overwhelmingly common cache-hit: no events, nothing to
    // reconcile, and we avoid even taking the memo write lock (so concurrent
    // marking reads don't serialize on it).
    if matches!(delta, MarkingDelta::Unchanged) {
        return;
    }
    let mut m = memo.write().unwrap();
    match delta {
        MarkingDelta::Unchanged => {}
        MarkingDelta::Rebuilt => match topology.get_topology() {
            Some(net) => m.rebuild_index(&net),
            None => m.clear_entries(),
        },
        MarkingDelta::Applied(events) => {
            // Fetch the net only when a (re)build is actually needed: the first
            // reconcile (index not yet built) or a structural NetInitialized in
            // the delta. The delta scan is over a handful of events and needs no
            // net, so a plain token-firing tick never clones the topology.
            let needs_net = !m.is_index_built()
                || events
                    .iter()
                    .any(|pe| matches!(pe.event, petri_domain::DomainEvent::NetInitialized { .. }));
            let net = if needs_net {
                topology.get_topology()
            } else {
                None
            };
            m.apply_events(net.as_ref(), events.iter().map(|pe| &pe.event));
        }
    }
}

/// Evaluate transitions until quiescence or max steps reached.
///
/// Fires enabled transitions one at a time, selecting by enabling time
/// (earliest first). Stops when no transitions are enabled or when
/// `max_steps` is reached.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn evaluate_until_quiescent<
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
>(
    events: &E,
    topology: &T,
    projection: &S,
    executor: &TransitionExecutor,
    effect_handlers: &RwLock<HashMap<String, Arc<dyn EffectHandler>>>,
    execution_mode: &RwLock<ExecutionMode>,
    replay_cursor: &RwLock<usize>,
    workflow_id_lock: &RwLock<Option<uuid::Uuid>>,
    cached_state: &RwLock<Option<(u64, Marking)>>,
    binding_memo: &RwLock<crate::binding_memo::BindingMemo>,
    max_steps: usize,
    schema_registry: Option<&SchemaRegistry>,
    secret_store: Option<&dyn SecretStore>,
    net_parameters: Option<&serde_json::Value>,
    pre_dispatch: Option<&PreDispatchRuntime>,
    dispatch_options: &petri_domain::DispatchOptions,
) -> Result<EvaluateResult, ServiceError> {
    let mut steps_executed = 0;
    let mut transitions_fired = Vec::new();
    let mut events_generated = Vec::new();

    while steps_executed < max_steps {
        let (marking, delta) = advance_marking(events, projection, cached_state).await;
        // Reconcile the negative-binding memo from the SAME event delta that
        // advanced the marking, so it can never disagree with `marking`.
        reconcile_binding_memo(binding_memo, topology, &delta);

        // Select next transition to fire
        let next_transition = select_next_transition(
            executor,
            topology,
            &marking,
            schema_registry,
            Some(binding_memo),
            SelectPhase::Normal,
        )?;

        match next_transition {
            None => {
                // No enabled transitions - quiescent
                let terminal_reached = check_terminal_state(topology, &marking);
                return Ok(EvaluateResult {
                    steps_executed,
                    transitions_fired,
                    final_state: EvaluateFinalState::Quiescent,
                    events: events_generated,
                    terminal_reached,
                    failure_reached: None,
                });
            }
            Some(transition_id) => {
                let wf_id = *workflow_id_lock.read().unwrap();
                // Snapshot the log position so a permanent failure can collect
                // the audit events the firing layer appended (EffectFailed /
                // ErrorOccurred / TokenRemoved) into the EvaluateResult. Use
                // the storage-order index (events.len()) and positional
                // slicing — the `.sequence` field is not safe to compare
                // across hydrated multi-session logs (see get_marking_cached).
                let idx_before = events.len().await;
                // Fire the transition
                let result = fire_transition::<E, T, S>(
                    events,
                    topology,
                    executor,
                    effect_handlers,
                    execution_mode,
                    replay_cursor,
                    wf_id,
                    &marking,
                    transition_id.clone(),
                    schema_registry,
                    secret_store,
                    net_parameters,
                    pre_dispatch,
                    dispatch_options,
                )
                .await;

                match result {
                    Ok(event) => {
                        transitions_fired.push((transition_id, event.sequence));
                        events_generated.push(event);
                        steps_executed += 1;
                    }
                    Err(ref e) if e.is_permanent() => {
                        // Permanent failure: re-firing with the same marking
                        // would deterministically fail again. The firing layer
                        // has ALREADY advanced the marking (consumed the
                        // offending tokens via EffectFailed{tokens_consumed} /
                        // TokenRemoved) and emitted the audit event. Do NOT
                        // re-append ErrorOccurred here — a second append would
                        // re-kick the consumer→eval bridge and double the
                        // audit, which is exactly the infinite-loop feedback we
                        // are eliminating. Stop the pass and report the failure
                        // so the eval-loop driver raises a net-level NetFailed
                        // marker and tears the net down.
                        let reason = format!("Transition {}: {}", transition_id, e);
                        tracing::warn!("{}", reason);
                        let retryable = matches!(
                            e,
                            ServiceError::EffectFailed {
                                retryable: true,
                                ..
                            }
                        );
                        // Surface the audit events the firing layer appended
                        // (so callers/tests see them in EvaluateResult.events;
                        // the driver also re-reads the store for SSE).
                        events_generated.extend(events.events_from(idx_before).await);
                        // Failure-path resource cleanup: release any lease/hold
                        // the net still carries before it is torn down. The
                        // finalizer drain fires each `t_<id>_finally` once,
                        // emitting the release to the pool net as a journaled
                        // event ahead of the driver's NetFailed — so a
                        // permanently-failed leased net no longer strands its
                        // runner/allocation (which would survive engine restart).
                        let finalizer_events = drain_finalizers::<E, T, S>(
                            events,
                            topology,
                            projection,
                            executor,
                            effect_handlers,
                            execution_mode,
                            replay_cursor,
                            wf_id,
                            cached_state,
                            binding_memo,
                            schema_registry,
                            secret_store,
                            net_parameters,
                            pre_dispatch,
                            dispatch_options,
                        )
                        .await;
                        events_generated.extend(finalizer_events);
                        return Ok(EvaluateResult {
                            steps_executed,
                            transitions_fired,
                            final_state: EvaluateFinalState::Quiescent,
                            events: events_generated,
                            terminal_reached: None,
                            failure_reached: Some(FailureInfo {
                                transition_id,
                                reason,
                                retryable,
                            }),
                        });
                    }
                    // Pre-dispatch Defer — marking unchanged, audit event
                    // already emitted by `fire_effect_transition`. Stop this
                    // pass; a future pass (triggered by new tokens / timers /
                    // next eval-notify) can re-attempt. The defer budget in
                    // `firing.rs` caps how many times a Defer can recur before
                    // it escalates to Reject.
                    Err(ServiceError::PreDispatchDeferred { .. }) => {
                        return Ok(EvaluateResult {
                            steps_executed,
                            transitions_fired,
                            final_state: EvaluateFinalState::Quiescent,
                            events: events_generated,
                            terminal_reached: None,
                            failure_reached: None,
                        });
                    }
                    // Pre-dispatch Reject — TERMINAL per spec § 6 ("the first
                    // hook returning Reject wins"). The previous treatment as
                    // a soft retryable outcome produced an infinite busy-loop:
                    // hook errors with `fail_open=false` → Reject → audit
                    // event appended → consumer→eval bridge re-kicks →
                    // re-fire → same Reject → … (200+/s, 286% CPU per stuck
                    // net). Reject must fail the net so the driver emits
                    // NetFailed and tears it down.
                    Err(ServiceError::PreDispatchRejected {
                        transition_id: tid,
                        hook_name,
                        reason,
                    }) => {
                        let synthesized = format!(
                            "Pre-dispatch hook '{}' rejected transition {}: {}",
                            hook_name, tid, reason
                        );
                        tracing::warn!(
                            transition_id = %tid,
                            hook = %hook_name,
                            reason = %reason,
                            "{}",
                            synthesized
                        );
                        // Release any held lease before teardown (same
                        // failure-path cleanup as the permanent-error arm).
                        let finalizer_events = drain_finalizers::<E, T, S>(
                            events,
                            topology,
                            projection,
                            executor,
                            effect_handlers,
                            execution_mode,
                            replay_cursor,
                            wf_id,
                            cached_state,
                            binding_memo,
                            schema_registry,
                            secret_store,
                            net_parameters,
                            pre_dispatch,
                            dispatch_options,
                        )
                        .await;
                        events_generated.extend(finalizer_events);
                        return Ok(EvaluateResult {
                            steps_executed,
                            transitions_fired,
                            final_state: EvaluateFinalState::Quiescent,
                            events: events_generated,
                            terminal_reached: None,
                            failure_reached: Some(FailureInfo {
                                transition_id: tid,
                                reason: synthesized,
                                retryable: false,
                            }),
                        });
                    }
                    // Non-permanent, non-soft errors (e.g. a benign
                    // GuardNotSatisfied race, a transient Internal): leave the
                    // net alive and end the pass. The driver logs and waits for
                    // the next notify.
                    Err(e) => return Err(e),
                }
            }
        }
    }

    // Reached max steps
    Ok(EvaluateResult {
        steps_executed,
        transitions_fired,
        final_state: EvaluateFinalState::LimitReached,
        events: events_generated,
        terminal_reached: None,
        failure_reached: None,
    })
}

/// Drain a permanently-failing net's **finalizer** transitions before it is
/// torn down, so any resource it still holds is released exactly-once on the
/// failure path too.
///
/// A finalizer (`Transition::finalizer == true`, e.g. a `LeaseScope`'s
/// `t_<id>_finally`) is never selected during normal evaluation — its only
/// input is the still-parked held token, which exists for the whole scope
/// lifetime, so a normal selection would steal the lease before the body runs.
/// On the SUCCESS path the scope's `t_<id>_exit` consumes that held token, so
/// here (Finalizing phase) the finalizer has no binding and the drain is a
/// no-op. On the FAILURE path `t_<id>_exit` could never fire (gated on body
/// completion), so the held token survives and the finalizer fires once,
/// emitting the release to the pool net.
///
/// Each finalizer fires through the ordinary `fire_transition` path, so its
/// release is a journaled `TransitionFired` (and the cross-net release bridge
/// is published) BEFORE the driver appends `NetFailed`. On replay the release
/// re-applies deterministically, so a restart never re-strands the unit. The
/// memo is intentionally bypassed (`memo: None`) — finalizers never enter it —
/// and the loop is bounded as a runaway backstop (one finalizer per held
/// resource in practice). A finalizer that itself errors stops the drain
/// (best-effort cleanup); the net still fails.
#[allow(clippy::too_many_arguments)]
async fn drain_finalizers<E: EventRepository, T: TopologyRepository, S: StateProjection>(
    events: &E,
    topology: &T,
    projection: &S,
    executor: &TransitionExecutor,
    effect_handlers: &RwLock<HashMap<String, Arc<dyn EffectHandler>>>,
    execution_mode: &RwLock<ExecutionMode>,
    replay_cursor: &RwLock<usize>,
    wf_id: Option<uuid::Uuid>,
    cached_state: &RwLock<Option<(u64, Marking)>>,
    binding_memo: &RwLock<crate::binding_memo::BindingMemo>,
    schema_registry: Option<&SchemaRegistry>,
    secret_store: Option<&dyn SecretStore>,
    net_parameters: Option<&serde_json::Value>,
    pre_dispatch: Option<&PreDispatchRuntime>,
    dispatch_options: &petri_domain::DispatchOptions,
) -> Vec<PersistedEvent> {
    /// Runaway backstop — far above any real net's held-resource count.
    const MAX_FINALIZER_STEPS: usize = 64;

    let mut fired = Vec::new();
    for _ in 0..MAX_FINALIZER_STEPS {
        let (marking, delta) = advance_marking(events, projection, cached_state).await;
        // Honor the "every advance_marking reconciles the memo" invariant even
        // on the teardown path (the net is about to die, but cached_state's
        // cursor advanced — keep the two in lockstep).
        reconcile_binding_memo(binding_memo, topology, &delta);

        let next = match select_next_transition(
            executor,
            topology,
            &marking,
            schema_registry,
            None,
            SelectPhase::Finalizing,
        ) {
            Ok(Some(t)) => t,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "finalizer selection failed during teardown; stopping drain");
                break;
            }
        };

        let idx_before = events.len().await;
        match fire_transition::<E, T, S>(
            events,
            topology,
            executor,
            effect_handlers,
            execution_mode,
            replay_cursor,
            wf_id,
            &marking,
            next.clone(),
            schema_registry,
            secret_store,
            net_parameters,
            pre_dispatch,
            dispatch_options,
        )
        .await
        {
            Ok(event) => fired.push(event),
            Err(e) => {
                tracing::warn!(
                    transition_id = %next,
                    error = %e,
                    "finalizer firing failed during teardown; stopping drain (net still fails)"
                );
                // Surface any audit events the firing layer appended.
                fired.extend(events.events_from(idx_before).await);
                break;
            }
        }
    }
    fired
}

/// Check if any terminal place in the topology has tokens.
///
/// Returns the first terminal place with a token, extracting the run result
/// from the token's data: the explicit `data.exit_code` envelope when present,
/// otherwise the whole token body (so hand-authored AIR nets that carry the
/// result inline still surface it instead of completing with empty outputs).
pub fn check_terminal_state(
    topology: &impl TopologyRepository,
    marking: &Marking,
) -> Option<TerminalReachedInfo> {
    let net = topology.get_topology()?;
    for place in net.places.values() {
        if !matches!(place.kind, PlaceKind::Terminal) {
            continue;
        }
        let tokens = marking.tokens_at(&place.id);
        if let Some(token) = tokens.first() {
            // The compiler's End node stamps an explicit `exit_code` envelope
            // ({ ok, value }); hand-authored AIR nets (e.g. clinic-submitted
            // scenarios) carry the result as the token body with no `exit_code`
            // key. Fall back to the whole token data so those nets surface a
            // result rather than completing with empty outputs.
            let exit_code = match &token.color {
                TokenColor::Data(data) => Some(
                    data.get("exit_code")
                        .cloned()
                        .unwrap_or_else(|| data.clone()),
                ),
                _ => None,
            };
            return Some(TerminalReachedInfo {
                place_id: place.id.to_string(),
                exit_code,
            });
        }
    }
    None
}

/// Thin wrapper that discards the cache-advance delta. Test-only; production
/// callers use [`advance_marking`] directly so they can reconcile the binding
/// memo from the delta.
#[cfg(test)]
pub(crate) async fn get_marking_cached<E: EventRepository, S: StateProjection>(
    events: &E,
    projection: &S,
    cached_state: &RwLock<Option<(u64, Marking)>>,
) -> Marking {
    advance_marking(events, projection, cached_state).await.0
}

/// How the marking cache advanced on a call to [`advance_marking`]. The eval
/// loop feeds this to the [`BindingMemo`](crate::binding_memo::BindingMemo) so
/// the negative-binding memo is invalidated from the **same** event delta that
/// moved the marking — never from a second, independently-timed read of the
/// log (which could disagree if an external append landed in between).
pub(crate) enum MarkingDelta {
    /// Cache hit — the marking is unchanged since the previous call.
    Unchanged,
    /// Incremental — exactly these events were applied to advance the marking.
    Applied(Vec<PersistedEvent>),
    /// Full reprojection — the cache was empty (first call, or after a reset /
    /// topology load). Treat as a wholesale change.
    Rebuilt,
}

/// Get the current marking *and* report how the cache advanced. The single
/// source of truth for the marking-cache cursor logic.
///
/// The cache cursor is the **storage-order count** of the event log
/// (`events.len()`), and incremental updates use positional slicing
/// (`events.events_from(idx)`). Filtering by the `.sequence` field is unsafe
/// here: a cache hydrated from a multi-session NATS stream may hold events
/// whose `.sequence` numbering restarts at 0 each session, so a sequence-field
/// filter silently drops live appends whose `.sequence` happens to fall below
/// the cursor — the cached marking then drifts away from `f(events)` and the
/// eval loop re-fires the same transition on a stale binding (the executor-net
/// infinite-fire bug, see [[engine-loop-dup-seq]]).
pub(crate) async fn advance_marking<E: EventRepository, S: StateProjection>(
    events: &E,
    projection: &S,
    cached_state: &RwLock<Option<(u64, Marking)>>,
) -> (Marking, MarkingDelta) {
    let current_idx = events.len().await as u64;

    enum CacheState {
        Hit(Marking),
        Stale {
            cached_idx: u64,
            cached_marking: Marking,
        },
        Miss,
    }

    let state = {
        let cache = cached_state.read().unwrap();
        match &*cache {
            Some((cached_idx, cached_marking)) if *cached_idx == current_idx => {
                CacheState::Hit(cached_marking.clone())
            }
            Some((cached_idx, cached_marking)) => CacheState::Stale {
                cached_idx: *cached_idx,
                cached_marking: cached_marking.clone(),
            },
            None => CacheState::Miss,
        }
    };

    match state {
        CacheState::Hit(marking) => (marking, MarkingDelta::Unchanged),
        CacheState::Stale {
            cached_idx,
            mut cached_marking,
        } => {
            let new_events = events.events_from(cached_idx as usize).await;
            for persisted in &new_events {
                crate::apply_event_to_marking(&mut cached_marking, &persisted.event);
            }
            *cached_state.write().unwrap() = Some((current_idx, cached_marking.clone()));
            (cached_marking, MarkingDelta::Applied(new_events))
        }
        CacheState::Miss => {
            let all = events.all_events().await;
            let marking = projection.project(&all);
            *cached_state.write().unwrap() = Some((current_idx, marking.clone()));
            (marking, MarkingDelta::Rebuilt)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::{Marking, PetriNet, Place, PlaceId, Token, TokenColor};

    /// Minimal topology repository for unit tests.
    struct TestTopology(Option<PetriNet>);

    impl TopologyRepository for TestTopology {
        fn get_topology(&self) -> Option<PetriNet> {
            self.0.clone()
        }
        fn set_topology(&self, _net: PetriNet) {}
        fn clear(&self) {}
        fn update_transition_script(
            &self,
            _id: &TransitionId,
            _script: String,
            _guard: Option<String>,
        ) -> bool {
            false
        }
    }

    fn net_with_terminal() -> PetriNet {
        let mut net = PetriNet::new();
        net.add_place(Place::internal("start"));
        net.add_place(Place::terminal("done"));
        net
    }

    #[test]
    fn check_terminal_empty_marking() {
        let topo = TestTopology(Some(net_with_terminal()));
        let marking = Marking::new();
        assert!(check_terminal_state(&topo, &marking).is_none());
    }

    #[test]
    fn check_terminal_unit_token() {
        let topo = TestTopology(Some(net_with_terminal()));
        let mut marking = Marking::new();
        marking.add_token(PlaceId("done".to_string()), Token::new(TokenColor::Unit));

        let result = check_terminal_state(&topo, &marking);
        assert!(result.is_some());
        let info = result.unwrap();
        assert_eq!(info.place_id, "done");
        assert!(info.exit_code.is_none());
    }

    #[test]
    fn check_terminal_data_token_with_exit_code() {
        let topo = TestTopology(Some(net_with_terminal()));
        let mut marking = Marking::new();
        marking.add_token(
            PlaceId("done".to_string()),
            Token::new(TokenColor::Data(serde_json::json!({"exit_code": 0}))),
        );

        let result = check_terminal_state(&topo, &marking).unwrap();
        assert_eq!(result.place_id, "done");
        assert_eq!(result.exit_code, Some(serde_json::json!(0)));
    }

    #[test]
    fn check_terminal_data_token_without_exit_code_falls_back_to_full_body() {
        let topo = TestTopology(Some(net_with_terminal()));
        let mut marking = Marking::new();
        marking.add_token(
            PlaceId("done".to_string()),
            Token::new(TokenColor::Data(serde_json::json!({"foo": "bar"}))),
        );

        let result = check_terminal_state(&topo, &marking).unwrap();
        assert_eq!(result.place_id, "done");
        // No explicit `exit_code` key → the whole token body is the result.
        assert_eq!(result.exit_code, Some(serde_json::json!({"foo": "bar"})));
    }

    #[test]
    fn check_terminal_no_terminal_places() {
        let mut net = PetriNet::new();
        net.add_place(Place::internal("a"));
        net.add_place(Place::signal("b"));
        let topo = TestTopology(Some(net));

        let mut marking = Marking::new();
        marking.add_token(PlaceId("a".to_string()), Token::new(TokenColor::Unit));
        marking.add_token(PlaceId("b".to_string()), Token::new(TokenColor::Unit));

        assert!(check_terminal_state(&topo, &marking).is_none());
    }

    #[test]
    fn check_terminal_multiple_terminal_places() {
        let mut net = PetriNet::new();
        net.add_place(Place::terminal("done"));
        net.add_place(Place::terminal("fail"));
        let topo = TestTopology(Some(net));

        // Only "fail" has a token
        let mut marking = Marking::new();
        marking.add_token(
            PlaceId("fail".to_string()),
            Token::new(TokenColor::Data(serde_json::json!({"exit_code": 1}))),
        );

        let result = check_terminal_state(&topo, &marking).unwrap();
        // Should find the one with a token
        assert_eq!(result.exit_code, Some(serde_json::json!(1)));
    }

    #[test]
    fn check_terminal_no_topology() {
        let topo = TestTopology(None);
        let marking = Marking::new();
        assert!(check_terminal_state(&topo, &marking).is_none());
    }

    // ── Decision cascade regression ─────────────────────────────────────
    //
    // A Decision lowers branches as a switch/case cascade: branch i fires
    // only when its own guard holds AND no higher-precedence guard did. This
    // test reproduces the bug class the cascade fixes: a lower-precedence
    // branch whose guard borrows upstream data gets an extra (read) input
    // arc, so the engine's rule-2 "specificity" (more input arcs) used to
    // override the declared order before priority was ever consulted.

    use petri_domain::{Arc as PetriArc, Port, Transition};

    fn cascade_net(branch1_guard: &str) -> PetriNet {
        let mut net = PetriNet::new();
        net.add_place(Place::internal("p_input"));
        net.add_place(Place::internal("p_data"));
        net.add_place(Place::internal("p_out0"));
        net.add_place(Place::internal("p_out1"));

        // branch 0: declared first, payload-only, ONE input arc, top priority.
        net.add_transition(
            Transition::new("t_dec_branch_0", "#{ output: input }")
                .with_input_port(Port::new("input"))
                .with_guard("(true)")
                .with_priority("4"),
        );
        // branch 1: declared second, borrows producer data via a read-arc
        // (TWO input arcs => higher rule-2 "specificity"), lower priority.
        net.add_transition(
            Transition::new("t_dec_branch_1", "#{ output: input }")
                .with_input_port(Port::new("input"))
                .with_input_port(Port::new("d_prod"))
                .with_guard(branch1_guard)
                .with_priority("3"),
        );

        let tb0 = TransitionId("t_dec_branch_0".to_string());
        let tb1 = TransitionId("t_dec_branch_1".to_string());
        net.add_arc(PetriArc::input(
            PlaceId("p_input".to_string()),
            tb0.clone(),
            "input",
        ));
        net.add_arc(PetriArc::output(
            tb0,
            "output",
            PlaceId("p_out0".to_string()),
        ));
        net.add_arc(PetriArc::input(
            PlaceId("p_input".to_string()),
            tb1.clone(),
            "input",
        ));
        net.add_arc(
            PetriArc::input(PlaceId("p_data".to_string()), tb1.clone(), "d_prod").with_read(true),
        );
        net.add_arc(PetriArc::output(
            tb1,
            "output",
            PlaceId("p_out1".to_string()),
        ));
        net
    }

    fn cascade_marking() -> Marking {
        // Identical created_at so rule-1 (enabling time) ties for both
        // branches, isolating the rule-2 vs declared-order question.
        let ts = chrono::Utc::now();
        let mut tok_in = Token::new(TokenColor::Data(serde_json::json!({ "k": 1 })));
        tok_in.created_at = ts;
        let mut tok_data = Token::new(TokenColor::Data(serde_json::json!({ "score": 10 })));
        tok_data.created_at = ts;
        let mut m = Marking::new();
        m.add_token(PlaceId("p_input".to_string()), tok_in);
        m.add_token(PlaceId("p_data".to_string()), tok_data);
        m
    }

    // ── Finalizer phase gate ────────────────────────────────────────────
    //
    // A finalizer transition (e.g. a LeaseScope's `t_<id>_finally`) is enabled
    // by its own input arc (the held token) for the whole scope lifetime, so it
    // must be invisible to NORMAL selection and visible ONLY to the post-failure
    // Finalizing drain. This is the gate `drain_finalizers` relies on to release
    // a held lease exactly-once on the failure path.

    fn finalizer_gate_net() -> PetriNet {
        let mut net = PetriNet::new();
        net.add_place(Place::internal("p_trigger"));
        net.add_place(Place::internal("p_held"));
        net.add_place(Place::internal("p_out"));
        net.add_place(Place::internal("p_release"));

        // Ordinary work transition — eligible only in Normal phase.
        net.add_transition(
            Transition::new("t_normal", "#{ output: input }").with_input_port(Port::new("input")),
        );
        // Finalizer — eligible only in Finalizing phase.
        net.add_transition(
            Transition::new("t_finally", "#{ release: held }")
                .with_input_port(Port::new("held"))
                .with_finalizer(true),
        );

        let t_normal = TransitionId("t_normal".to_string());
        let t_finally = TransitionId("t_finally".to_string());
        net.add_arc(PetriArc::input(
            PlaceId("p_trigger".to_string()),
            t_normal.clone(),
            "input",
        ));
        net.add_arc(PetriArc::output(
            t_normal,
            "output",
            PlaceId("p_out".to_string()),
        ));
        net.add_arc(PetriArc::input(
            PlaceId("p_held".to_string()),
            t_finally.clone(),
            "held",
        ));
        net.add_arc(PetriArc::output(
            t_finally,
            "release",
            PlaceId("p_release".to_string()),
        ));
        net
    }

    fn finalizer_gate_marking(trigger: bool, held: bool) -> Marking {
        let mut m = Marking::new();
        if trigger {
            m.add_token(
                PlaceId("p_trigger".to_string()),
                Token::new(TokenColor::Data(serde_json::json!({ "k": 1 }))),
            );
        }
        if held {
            m.add_token(
                PlaceId("p_held".to_string()),
                Token::new(TokenColor::Data(serde_json::json!({ "grant_id": "g1" }))),
            );
        }
        m
    }

    #[test]
    fn finalizer_never_selected_in_normal_phase() {
        let exec = crate::TransitionExecutor::new();
        let topo = TestTopology(Some(finalizer_gate_net()));

        // Both enabled: Normal selects the ordinary transition, never the finalizer.
        let picked = select_next_transition(
            &exec,
            &topo,
            &finalizer_gate_marking(true, true),
            None,
            None,
            SelectPhase::Normal,
        )
        .unwrap();
        assert_eq!(picked, Some(TransitionId("t_normal".to_string())));

        // Only the held token present (the body is gone): Normal finds NOTHING —
        // the finalizer is gated out even though its input arc is satisfied.
        let picked = select_next_transition(
            &exec,
            &topo,
            &finalizer_gate_marking(false, true),
            None,
            None,
            SelectPhase::Normal,
        )
        .unwrap();
        assert_eq!(
            picked, None,
            "a finalizer must be invisible to normal selection even when enabled"
        );
    }

    #[test]
    fn finalizing_phase_selects_only_finalizers() {
        let exec = crate::TransitionExecutor::new();
        let topo = TestTopology(Some(finalizer_gate_net()));

        // Held token present: the Finalizing drain picks the finalizer.
        let picked = select_next_transition(
            &exec,
            &topo,
            &finalizer_gate_marking(true, true),
            None,
            None,
            SelectPhase::Finalizing,
        )
        .unwrap();
        assert_eq!(
            picked,
            Some(TransitionId("t_finally".to_string())),
            "Finalizing must release the held lease, ignoring the still-enabled ordinary transition"
        );

        // Held already released (success path consumed it): the drain is a no-op
        // even though the ordinary transition is still enabled.
        let picked = select_next_transition(
            &exec,
            &topo,
            &finalizer_gate_marking(true, false),
            None,
            None,
            SelectPhase::Finalizing,
        )
        .unwrap();
        assert_eq!(
            picked, None,
            "Finalizing must ignore non-finalizer transitions entirely"
        );
    }

    #[test]
    fn cascade_keeps_declaration_order_despite_specificity() {
        let exec = crate::TransitionExecutor::new();
        let marking = cascade_marking();

        // Compiled cascade form: branch 1 = (its guard) && !(branch 0 guard).
        // Branch 0's guard is `true`, so branch 1's guard is always false and
        // branch 1 is never enabled — the topmost branch wins even though it
        // has FEWER input arcs and the engine's specificity rule would favor
        // branch 1.
        let topo = TestTopology(Some(cascade_net("(d_prod.score > 0) && !(true)")));
        let picked =
            select_next_transition(&exec, &topo, &marking, None, None, SelectPhase::Normal).unwrap();
        assert_eq!(
            picked,
            Some(TransitionId("t_dec_branch_0".to_string())),
            "cascade must keep declared precedence (branch 0) regardless of arc count"
        );

        // Pre-fix lowering (no cascade exclusion): branch 1's guard is just
        // its own condition. Now both are enabled, and the lower-precedence
        // branch 1 wins purely on rule-2 specificity (2 input arcs > 1),
        // beating branch 0's higher priority. This is the regression.
        let topo_buggy = TestTopology(Some(cascade_net("d_prod.score > 0")));
        let picked_buggy =
            select_next_transition(&exec, &topo_buggy, &marking, None, None, SelectPhase::Normal)
                .unwrap();
        assert_eq!(
            picked_buggy,
            Some(TransitionId("t_dec_branch_1".to_string())),
            "sanity: without the cascade, specificity overrides declared order"
        );
    }

    /// Regression for [[engine-loop-dup-seq]]: when the event log holds
    /// hydrated events whose `.sequence` field restarts at 0 across
    /// sessions, `get_marking_cached` must still satisfy
    /// `marking == project(all_events())` after every live append. The
    /// pre-fix implementation cursored by `.sequence`, so the Stale path
    /// silently dropped any newly-appended event whose `.sequence` happened
    /// to fall below the cursor — the cache then permanently disagreed with
    /// the event log and the eval loop re-fired the same transition on a
    /// stale binding.
    #[tokio::test]
    async fn get_marking_cached_matches_full_projection_under_dup_sequences() {
        use petri_domain::{DomainEvent, PersistedEvent, PlaceId, Token, TokenColor, TokenId};

        /// Test repo that lets us inject events with arbitrary `.sequence`
        /// (simulating hydration of a multi-session NATS stream) AND uses
        /// `Vec`-position semantics for `len`/`events_from`. This is the
        /// shape `MemoryEventStore` has after the fix.
        struct DupSeqRepo {
            events: std::sync::RwLock<Vec<PersistedEvent>>,
            next_seq: std::sync::Mutex<u64>,
        }
        impl DupSeqRepo {
            fn new() -> Self {
                Self {
                    events: std::sync::RwLock::new(Vec::new()),
                    next_seq: std::sync::Mutex::new(0),
                }
            }
            fn load(&self, ev: PersistedEvent) {
                self.events.write().unwrap().push(ev);
            }
        }
        #[async_trait::async_trait]
        impl crate::EventRepository for DupSeqRepo {
            async fn append(
                &self,
                event: DomainEvent,
            ) -> Result<PersistedEvent, crate::EventStoreError> {
                let mut next = self.next_seq.lock().unwrap();
                let seq = *next;
                *next += 1;
                let mut events = self.events.write().unwrap();
                let prev_hash = events.last().map(|e| e.hash.clone());
                let p = PersistedEvent::new(seq, event, prev_hash);
                events.push(p.clone());
                Ok(p)
            }
            async fn all_events(&self) -> Vec<PersistedEvent> {
                self.events.read().unwrap().clone()
            }
            async fn events_since(&self, sequence: u64) -> Vec<PersistedEvent> {
                self.events
                    .read()
                    .unwrap()
                    .iter()
                    .filter(|e| e.sequence >= sequence)
                    .cloned()
                    .collect()
            }
            async fn reset(&self) {
                self.events.write().unwrap().clear();
                *self.next_seq.lock().unwrap() = 0;
            }
            async fn current_sequence(&self) -> u64 {
                self.events.read().unwrap().len() as u64
            }
            async fn len(&self) -> usize {
                self.events.read().unwrap().len()
            }
            async fn events_from(&self, idx: usize) -> Vec<PersistedEvent> {
                let g = self.events.read().unwrap();
                let s = idx.min(g.len());
                g[s..].to_vec()
            }
        }

        struct Proj;
        impl crate::StateProjection for Proj {
            fn project(&self, events: &[PersistedEvent]) -> Marking {
                let mut m = Marking::new();
                for p in events {
                    crate::apply_event_to_marking(&mut m, &p.event);
                }
                m
            }
        }

        let repo = DupSeqRepo::new();
        let proj = Proj;
        let cache = std::sync::RwLock::new(None);
        let place = PlaceId::named("p");

        // -- Hydration phase: two prior sessions, sequences restart at 0.
        let mk_create = |seq: u64, tid: TokenId| {
            PersistedEvent::new(
                seq,
                DomainEvent::TokenCreated {
                    token: Token {
                        id: tid,
                        color: TokenColor::Unit,
                        created_at: chrono::Utc::now(),
                        created_by_event: None,
                        reply_routing: None,
                    },
                    place_id: place.clone(),
                    place_name: None,
                    workflow_id: None,
                    signal_key: None,
                    dedup_id: None,
                },
                None,
            )
        };
        let mk_consume = |seq: u64, tid: TokenId| {
            PersistedEvent::new(
                seq,
                DomainEvent::TransitionFired {
                    transition_id: TransitionId::named("t"),
                    transition_name: None,
                    consumed_tokens: vec![(place.clone(), tid)],
                    produced_tokens: vec![],
                    read_tokens: vec![],
                    process_step_started: None,
                    process_step_completed: None,
                },
                None,
            )
        };

        let tok_a = TokenId::new();
        let tok_b = TokenId::new();
        // Session 1: create-then-consume, sequences 0,1
        repo.load(mk_create(0, tok_a.clone()));
        repo.load(mk_consume(1, tok_a));
        // Session 2: create-then-consume, sequences 0,1 AGAIN (overlap)
        repo.load(mk_create(0, tok_b.clone()));
        repo.load(mk_consume(1, tok_b));

        // -- First call: Miss path projects all 4 events → marking should be
        // empty (both creates consumed in-session). This is where the cursor
        // gets seeded.
        let m0 = get_marking_cached(&repo, &proj, &cache).await;
        assert_eq!(m0.token_count(&place), 0, "hydrated sessions cancel out");

        // -- Session 3 begins: a fresh live append seeds a token. Crucially,
        // its `.sequence` is FAR BELOW the cursor (cursor is 4, this event
        // has next_seq=0 because the test repo restarts). The Stale path
        // must still pick it up.
        let new_tok = TokenId::new();
        repo.append(DomainEvent::TokenCreated {
            token: Token {
                id: new_tok.clone(),
                color: TokenColor::Unit,
                created_at: chrono::Utc::now(),
                created_by_event: None,
                reply_routing: None,
            },
            place_id: place.clone(),
            place_name: None,
            workflow_id: None,
            signal_key: None,
            dedup_id: None,
        })
        .await
        .expect("append");

        let m1 = get_marking_cached(&repo, &proj, &cache).await;
        let full = proj.project(&repo.all_events().await);
        assert_eq!(
            m1.token_count(&place),
            full.token_count(&place),
            "marking = f(events) invariant must hold across the Stale path even when live appends carry a `.sequence` below the cursor (this fails with the pre-fix events_since-based cursor)"
        );
        assert_eq!(m1.token_count(&place), 1);

        // -- Same again: a consume should retract the token, regardless of
        // its `.sequence` field relative to the cursor.
        repo.append(DomainEvent::TransitionFired {
            transition_id: TransitionId::named("t"),
            transition_name: None,
            consumed_tokens: vec![(place.clone(), new_tok)],
            produced_tokens: vec![],
            read_tokens: vec![],
            process_step_started: None,
            process_step_completed: None,
        })
        .await
        .expect("append");

        let m2 = get_marking_cached(&repo, &proj, &cache).await;
        let full2 = proj.project(&repo.all_events().await);
        assert_eq!(
            m2.token_count(&place),
            full2.token_count(&place),
            "Stale path must reach quiescence after consume"
        );
        assert_eq!(m2.token_count(&place), 0);
    }

    #[test]
    fn deadend_throw_is_a_permanent_script_error() {
        // The synthesized `t_{id}_deadend` transition raises via Rhai `throw`.
        // execute_script must surface that as a permanent ServiceError so the
        // firing path emits ErrorOccurred and consumes the token (rather than
        // stranding it or re-firing forever).
        let exec = crate::TransitionExecutor::new();
        let err = exec
            .execute_script(
                "throw \"decision X: token matched no branch and no default branch\"",
                &std::collections::HashMap::new(),
            )
            .expect_err("throw must produce an error");
        assert!(
            matches!(err, crate::ServiceError::ScriptError { .. }),
            "expected ScriptError, got {err:?}"
        );
        assert!(err.is_permanent(), "dead-end error must be permanent");
    }
}
