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

/// Select the next transition to fire based on enabling time, specificity, and token priority.
///
/// Priority order:
/// 1. Earliest enabling time (oldest tokens)
/// 2. More specific transitions (more input arcs)
/// 3. Higher token priority (from priority expression)
/// 4. Transition ID (alphabetical tiebreaker)
pub(crate) fn select_next_transition(
    executor: &TransitionExecutor,
    topology: &impl TopologyRepository,
    marking: &Marking,
    schema_registry: Option<&SchemaRegistry>,
) -> Result<Option<TransitionId>, ServiceError> {
    let net = topology.get_topology().ok_or(ServiceError::NoTopology)?;

    let mut best_id: Option<TransitionId> = None;
    let mut best_time: Option<DateTime<Utc>> = None;
    let mut best_input_count: usize = 0;
    let mut best_priority: Option<f64> = None;

    for transition in net.transitions.values() {
        let input_arcs = net.input_arcs(&transition.id);

        // Find a valid binding for this transition
        if let Some(binding) =
            find_valid_binding(executor, transition, &input_arcs, marking, schema_registry)
        {
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
                                    (Some(p), Some(bp)) if p > bp => true,
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

    Ok(best_id)
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
    max_steps: usize,
    schema_registry: Option<&SchemaRegistry>,
    secret_store: Option<&dyn SecretStore>,
    net_parameters: Option<&serde_json::Value>,
    pre_dispatch: Option<&PreDispatchRuntime>,
) -> Result<EvaluateResult, ServiceError> {
    let mut steps_executed = 0;
    let mut transitions_fired = Vec::new();
    let mut events_generated = Vec::new();

    while steps_executed < max_steps {
        let marking = get_marking_cached(events, projection, cached_state).await;

        // Select next transition to fire
        let next_transition =
            select_next_transition(executor, topology, &marking, schema_registry)?;

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
                // ErrorOccurred / TokenRemoved) into the EvaluateResult.
                let seq_before = events.current_sequence().await;
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
                        let retryable =
                            matches!(e, ServiceError::EffectFailed { retryable: true, .. });
                        // Surface the audit events the firing layer appended
                        // (so callers/tests see them in EvaluateResult.events;
                        // the driver also re-reads the store for SSE).
                        events_generated.extend(events.events_since(seq_before).await);
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
                    // Pre-dispatch soft outcomes — marking unchanged, audit
                    // events already emitted by `fire_effect_transition`.
                    // Stop this evaluation pass; a future pass (triggered by
                    // new tokens / timers / next eval-notify) can re-attempt.
                    Err(
                        ServiceError::PreDispatchRejected { .. }
                        | ServiceError::PreDispatchDeferred { .. },
                    ) => {
                        return Ok(EvaluateResult {
                            steps_executed,
                            transitions_fired,
                            final_state: EvaluateFinalState::Quiescent,
                            events: events_generated,
                            terminal_reached: None,
                            failure_reached: None,
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

/// Check if any terminal place in the topology has tokens.
///
/// Returns the first terminal place with a token, extracting an exit code
/// from the token's data if present (looks for `data.exit_code`).
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
            let exit_code = match &token.color {
                TokenColor::Data(data) => data.get("exit_code").cloned(),
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
    fn check_terminal_data_token_without_exit_code() {
        let topo = TestTopology(Some(net_with_terminal()));
        let mut marking = Marking::new();
        marking.add_token(
            PlaceId("done".to_string()),
            Token::new(TokenColor::Data(serde_json::json!({"foo": "bar"}))),
        );

        let result = check_terminal_state(&topo, &marking).unwrap();
        assert_eq!(result.place_id, "done");
        assert!(result.exit_code.is_none());
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
}

/// Get the current marking, using a cache to avoid full event replay.
pub(crate) async fn get_marking_cached<E: EventRepository, S: StateProjection>(
    events: &E,
    projection: &S,
    cached_state: &RwLock<Option<(u64, Marking)>>,
) -> Marking {
    let current_seq = events.current_sequence().await;

    enum CacheState {
        Hit(Marking),
        Stale {
            cached_seq: u64,
            cached_marking: Marking,
        },
        Miss,
    }

    let state = {
        let cache = cached_state.read().unwrap();
        match &*cache {
            Some((cached_seq, cached_marking)) if *cached_seq == current_seq => {
                CacheState::Hit(cached_marking.clone())
            }
            Some((cached_seq, cached_marking)) => CacheState::Stale {
                cached_seq: *cached_seq,
                cached_marking: cached_marking.clone(),
            },
            None => CacheState::Miss,
        }
    };

    match state {
        CacheState::Hit(marking) => marking,
        CacheState::Stale {
            cached_seq,
            mut cached_marking,
        } => {
            let new_events = events.events_since(cached_seq).await;
            for persisted in &new_events {
                crate::apply_event_to_marking(&mut cached_marking, &persisted.event);
            }
            *cached_state.write().unwrap() = Some((current_seq, cached_marking.clone()));
            cached_marking
        }
        CacheState::Miss => {
            let all = events.all_events().await;
            let marking = projection.project(&all);
            *cached_state.write().unwrap() = Some((current_seq, marking.clone()));
            marking
        }
    }
}
