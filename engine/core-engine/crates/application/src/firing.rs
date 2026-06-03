use std::collections::HashMap;
use std::sync::RwLock;

use petri_domain::{
    DomainEvent, Marking, PersistedEvent, PetriNet, PlaceId, ReplyRouting, Token, Transition,
    TransitionId,
};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use aithericon_secrets::SecretStore;

use crate::binding::find_valid_binding;
use crate::effect::{EffectHandler, EffectInput, ExecutionMode};
use crate::pre_dispatch::{
    evaluate_chain, ChainEvalInputs, ChainEvalOutcome, PreDispatchMetadata, PreDispatchRuntime,
};
use crate::rhai_runtime::json_to_token_color;
use crate::schema_registry::SchemaRegistry;
use crate::{
    EventRepository, ServiceError, StateProjection, TopologyRepository, TransitionExecutor,
};

use std::sync::Arc;

use crate::binding::TokenBinding;

/// Collect the read-arc inputs for an effect from a binding.
///
/// These are the entries in `port_inputs` that arrived via non-consuming
/// read arcs (named in `read_port_names`). Shared by the live and replay
/// effect-firing paths so they cannot drift.
fn collect_read_inputs(binding: &TokenBinding) -> HashMap<String, JsonValue> {
    binding
        .read_port_names
        .iter()
        .filter_map(|name| {
            binding
                .port_inputs
                .get(name)
                .map(|v| (name.clone(), v.clone()))
        })
        .collect()
}

/// Select the process-step label for an effect, preferring the
/// `started` step and falling back to the `completed` step.
fn select_process_step(transition: &Transition) -> Option<String> {
    transition
        .process_step_started
        .clone()
        .or_else(|| transition.process_step_completed.clone())
}

/// Resolve a bridge target field.
///
/// - `$params.key` → look up from net parameters (set at net creation time)
/// - `$result.key` → look up from effect handler result (available after effect execution)
/// - anything else → literal string
fn resolve_param(
    field: &str,
    params: Option<&JsonValue>,
    effect_result: Option<&JsonValue>,
) -> Result<String, ServiceError> {
    if let Some(key) = field.strip_prefix("$result.") {
        let result = effect_result.ok_or_else(|| {
            ServiceError::Internal(format!(
                "Bridge target '{}' references $result but no effect result available",
                field
            ))
        })?;
        result
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                ServiceError::Internal(format!(
                    "Effect result key '{}' not found or not a string",
                    key
                ))
            })
    } else if let Some(key) = field.strip_prefix("$params.") {
        let params = params.ok_or_else(|| {
            ServiceError::Internal(format!(
                "Bridge target '{}' references $params but net has no parameters",
                field
            ))
        })?;
        params
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| {
                ServiceError::Internal(format!("Net parameter '{}' not found or not a string", key))
            })
    } else {
        Ok(field.to_string())
    }
}

/// Route output tokens through bridge/claims logic.
///
/// Shared by both `fire_transition` (Rhai script results) and
/// `fire_effect_transition` (effect handler results).
///
/// Returns `(produced_tokens, bridge_out_tokens)`.
#[allow(clippy::type_complexity)]
pub(crate) fn route_output_tokens(
    net: &PetriNet,
    transition: &Transition,
    transition_id: &TransitionId,
    script_result: HashMap<String, JsonValue>,
    consumed_reply_routing: &Option<ReplyRouting>,
    schema_registry: Option<&SchemaRegistry>,
    net_parameters: Option<&JsonValue>,
    effect_result: Option<&JsonValue>,
) -> Result<
    (
        Vec<(PlaceId, Token)>,
        Vec<(
            PlaceId,
            Token,
            petri_domain::BridgeTarget,
            String,
            Option<String>,
        )>,
    ),
    ServiceError,
> {
    let mut produced_tokens: Vec<(PlaceId, Token)> = Vec::new();
    let mut bridge_out_tokens: Vec<(
        PlaceId,
        Token,
        petri_domain::BridgeTarget,
        String,
        Option<String>,
    )> = Vec::new();

    for (port_name, token_data) in script_result {
        let port = transition.output_port(&port_name);
        if port.is_none() {
            return Err(ServiceError::UnknownOutputPort {
                port_name: port_name.clone(),
            });
        }

        // SCATTER: a Batch-cardinality output port unwraps its array value into
        // ONE token per element (preserving array order). A Single port (the
        // default) yields exactly one token carrying the whole value — byte
        // identical to the pre-scatter behavior. A non-array value on a Batch
        // output port is a permanent error that advances the marking.
        let is_batch = matches!(
            port.map(|p| &p.cardinality),
            Some(petri_domain::PortCardinality::Batch)
        );
        let element_values: Vec<JsonValue> = if is_batch {
            match token_data {
                JsonValue::Array(arr) => arr,
                _ => {
                    return Err(ServiceError::BatchOutputNotArray {
                        port_name: port_name.clone(),
                    });
                }
            }
        } else {
            vec![token_data]
        };

        for token_data in element_values {
            // Validate output token against schema (skip _error port). For a
            // Batch port this validates EACH ELEMENT against schema_ref (the
            // element/item shape), which is the correct contract.
            if port_name != "_error" {
                if let (Some(registry), Some(port)) = (schema_registry, port) {
                    if let Some(ref schema_ref) = port.schema_ref {
                        if let Err(e) = registry.validate(schema_ref, &token_data) {
                            return Err(ServiceError::SchemaValidationFailed {
                                port_name: port_name.clone(),
                                transition_id: transition_id.clone(),
                                error: e.to_string(),
                            });
                        }
                    }
                }
            }

            let output_arc = net
                .output_arc_for_port(transition_id, &port_name)
                .ok_or_else(|| ServiceError::NoArcForPort {
                    port_name: port_name.clone(),
                })?;

            let token_color = json_to_token_color(&token_data);
            let mut token = Token::new(token_color);

            if let Some(place) = net.get_place(&output_arc.place_id) {
                match &place.kind {
                    petri_domain::PlaceKind::BridgeReply { channel } => {
                        let reply_addr = consumed_reply_routing.as_ref().and_then(|meta| {
                            if let Some(ch) = channel {
                                meta.reply_channels
                                    .as_ref()
                                    .and_then(|m| m.get(ch.as_str()))
                            } else {
                                meta.reply_to.as_ref()
                            }
                        });

                        match reply_addr {
                            Some(addr) => {
                                bridge_out_tokens.push((
                                    output_arc.place_id.clone(),
                                    token,
                                    petri_domain::BridgeTarget {
                                        target_net_id: addr.net_id.clone(),
                                        target_place_name: addr.place_name.clone(),
                                        reply_to: None,
                                        reply_channels: None,
                                    },
                                    place.name.clone(),
                                    None,
                                ));
                                continue;
                            }
                            None => {
                                return Err(ServiceError::BridgeReplyMissing {
                                    place_name: place.name.clone(),
                                    channel: channel.clone(),
                                });
                            }
                        }
                    }
                    petri_domain::PlaceKind::BridgeOut {
                        target_net_id,
                        target_place_name,
                        reply_to,
                        reply_channels,
                        ..
                    } => {
                        let resolved_net =
                            resolve_param(target_net_id, net_parameters, effect_result)?;
                        let resolved_place =
                            resolve_param(target_place_name, net_parameters, effect_result)?;
                        bridge_out_tokens.push((
                            output_arc.place_id.clone(),
                            token,
                            petri_domain::BridgeTarget {
                                target_net_id: resolved_net,
                                target_place_name: resolved_place,
                                reply_to: reply_to.clone(),
                                reply_channels: reply_channels.clone(),
                            },
                            place.name.clone(),
                            reply_to.clone(),
                        ));
                        continue;
                    }
                    _ => {
                        // Output tokens inherit the firing's merged consumed
                        // reply-routing — UNLESS this arc opts out via
                        // `reset_reply_routing`. A recycled resource token (e.g.
                        // a presence pool's freed unit) opts out so it returns
                        // routing-less; otherwise a later grant binding would
                        // merge the stale (holder) reply channel with the next
                        // claim's, hit a conflict, and skip the binding —
                        // wedging re-grant. See `Arc::reset_reply_routing`.
                        if !output_arc.reset_reply_routing {
                            if let Some(ref meta) = consumed_reply_routing {
                                token = token.with_reply_routing(meta.clone());
                            }
                        }
                    }
                }
            }

            produced_tokens.push((output_arc.place_id.clone(), token));
        }
    }

    Ok((produced_tokens, bridge_out_tokens))
}

/// Emit bridge-out events for tokens routed to remote nets.
///
/// `produced_by_event` is the sequence number of the TransitionFired /
/// EffectCompleted event that produced these tokens. The causality consumer
/// uses it to walk back to the producing event and inherit process tags
/// from consumed tokens.
pub(crate) async fn emit_bridge_out_events<E: EventRepository>(
    events: &E,
    transition_id: &TransitionId,
    bridge_out_tokens: Vec<(
        PlaceId,
        Token,
        petri_domain::BridgeTarget,
        String,
        Option<String>,
    )>,
    produced_by_event: Option<u64>,
) -> Result<(), ServiceError> {
    for (place_id, token, target, place_name, reply_to_place_name) in bridge_out_tokens {
        events
            .append(DomainEvent::TokenBridgedOut {
                token,
                source_place_id: place_id,
                source_place_name: place_name,
                target_net_id: target.target_net_id,
                target_place_name: target.target_place_name,
                transition_id: transition_id.clone(),
                signal_key: Uuid::new_v4().to_string(),
                produced_by_event,
                reply_to_place_name,
                reply_channels: target.reply_channels,
            })
            .await?;
    }
    Ok(())
}

/// On a **permanent** failure of a Rhai (non-effect) transition, consume the
/// bound input tokens so the marking advances and the broken transition is no
/// longer selected — otherwise the consumer→eval bridge re-kicks the loop and
/// the same transition re-fails forever. Then propagate the original error.
///
/// Non-permanent failures (benign races, transient internals) are propagated
/// untouched: the net stays alive and the eval loop merely ends the current
/// pass. Pure-Rhai transitions have no effect handler, so there is no replay
/// cursor to keep in sync — `TokenRemoved` is the right marking-advancing event
/// here (it is already projected and NATS/CLI-mapped).
///
/// A single `ErrorOccurred` is also emitted so the human-readable failure stays
/// visible in the event log and `aithericon errors` (the effect path gets this
/// visibility from its `EffectFailed` event; the Rhai path has none otherwise).
/// This was the event that previously re-kicked the eval loop forever — it is
/// safe now precisely because the accompanying `TokenRemoved`s advance the
/// marking, so the broken transition is no longer enabled on the next pass.
async fn consume_bound_tokens_on_permanent_failure<E: EventRepository>(
    events: &E,
    consumed_tokens: &[(PlaceId, petri_domain::TokenId)],
    err: ServiceError,
) -> Result<PersistedEvent, ServiceError> {
    if err.is_permanent() {
        let reason = format!("permanent transition failure: {}", err);
        events
            .append(DomainEvent::ErrorOccurred {
                message: reason.clone(),
            })
            .await?;
        for (place_id, token_id) in consumed_tokens {
            events
                .append(DomainEvent::TokenRemoved {
                    token_id: token_id.clone(),
                    place_id: place_id.clone(),
                    reason: Some(reason.clone()),
                    correlation_id: None,
                })
                .await?;
        }
    }
    Err(err)
}

/// Fire a transition using port-based routing.
///
/// The execution flow:
/// 1. Find a valid token binding (searches combinations if guard present)
/// 2. If effect transition -> delegate to `fire_effect_transition()`
/// 3. Execute main script with the bound tokens
/// 4. Route output tokens based on script result
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fire_transition<
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
>(
    events: &E,
    topology: &T,
    executor: &TransitionExecutor,
    effect_handlers: &RwLock<HashMap<String, Arc<dyn EffectHandler>>>,
    execution_mode: &RwLock<ExecutionMode>,
    replay_cursor: &RwLock<usize>,
    workflow_id: Option<Uuid>,
    marking: &Marking,
    transition_id: TransitionId,
    schema_registry: Option<&SchemaRegistry>,
    secret_store: Option<&dyn SecretStore>,
    net_parameters: Option<&JsonValue>,
    pre_dispatch: Option<&PreDispatchRuntime>,
    dispatch_options: &petri_domain::DispatchOptions,
) -> Result<PersistedEvent, ServiceError> {
    let net = topology.get_topology().ok_or(ServiceError::NoTopology)?;

    let transition = net
        .get_transition(&transition_id)
        .ok_or_else(|| ServiceError::TransitionNotFound(transition_id.clone()))?
        .clone();

    // Sub-phase 2.5e-γ.mekhan: skip branch. If the transition_id is in the
    // per-run skip_mask, consume inputs + emit TransitionSkipped event with
    // Token::new_unit() defaults on each declared output port place. No
    // effect dispatch, no Rhai logic, no pre-dispatch hook chain. Honest
    // semantics for research-harness ablation per
    // `project_three_use_cases_and_visualization`.
    //
    // Scaffold-stage: the guard + dispatch is wired; `execute_skip` body
    // ships in sub-phase 2.5e-γ.mekhan-S1 (per scaffold-then-dispatch
    // pattern). Until S1 lands, the placeholder panics — the guard's
    // condition is only satisfied when a client explicitly populates
    // skip_mask, so no live consumer trips this path at scaffold time.
    if dispatch_options
        .skip_mask
        .iter()
        .any(|id| id == &transition_id.0)
    {
        return execute_skip(
            events,
            executor,
            &net,
            marking,
            &transition,
            &transition_id,
            schema_registry,
        )
        .await;
    }

    // Branch: effect transitions use a separate path
    if transition.is_effect() {
        return fire_effect_transition::<E, T, S>(
            events,
            topology,
            executor,
            effect_handlers,
            execution_mode,
            replay_cursor,
            workflow_id,
            marking,
            &net,
            &transition,
            schema_registry,
            secret_store,
            net_parameters,
            pre_dispatch,
            dispatch_options,
        )
        .await;
    }

    let input_arcs = net.input_arcs(&transition_id);
    let binding = find_valid_binding(executor, &transition, &input_arcs, marking, schema_registry)
        .ok_or_else(|| ServiceError::GuardNotSatisfied(transition_id.clone()))?;

    let script_result = match executor.execute_script(&transition.script, &binding.port_inputs) {
        Ok(r) => r,
        Err(e) => {
            return consume_bound_tokens_on_permanent_failure(events, &binding.consumed_tokens, e)
                .await
        }
    };

    let (produced_tokens, bridge_out_tokens) = match route_output_tokens(
        &net,
        &transition,
        &transition_id,
        script_result,
        &binding.consumed_reply_routing,
        schema_registry,
        net_parameters,
        None, // Rhai transitions have no effect result
    ) {
        Ok(v) => v,
        Err(e) => {
            return consume_bound_tokens_on_permanent_failure(events, &binding.consumed_tokens, e)
                .await
        }
    };

    let event = events
        .append(DomainEvent::TransitionFired {
            transition_id: transition_id.clone(),
            transition_name: Some(transition.name.clone()),
            consumed_tokens: binding.consumed_tokens,
            produced_tokens,
            read_tokens: binding.read_tokens,
            process_step_started: transition.process_step_started.clone(),
            process_step_completed: transition.process_step_completed.clone(),
        })
        .await?;

    emit_bridge_out_events(
        events,
        &transition_id,
        bridge_out_tokens,
        Some(event.sequence),
    )
    .await?;

    Ok(event)
}

/// Fire an effect transition.
///
/// - **Live mode**: Build `EffectInput` -> look up handler -> call `execute()` ->
///   convert output -> emit `EffectCompleted` -> route output tokens.
/// - **Replay mode**: Find next `EffectCompleted` event -> call `handler.replay()`
///   to rebuild owned state -> use stored produced_tokens directly.
#[allow(clippy::too_many_arguments, clippy::extra_unused_type_parameters)]
async fn fire_effect_transition<E: EventRepository, T: TopologyRepository, S: StateProjection>(
    events: &E,
    _topology: &T,
    executor: &TransitionExecutor,
    effect_handlers: &RwLock<HashMap<String, Arc<dyn EffectHandler>>>,
    execution_mode: &RwLock<ExecutionMode>,
    replay_cursor: &RwLock<usize>,
    _workflow_id: Option<Uuid>,
    marking: &Marking,
    net: &PetriNet,
    transition: &Transition,
    schema_registry: Option<&SchemaRegistry>,
    secret_store: Option<&dyn SecretStore>,
    net_parameters: Option<&JsonValue>,
    pre_dispatch: Option<&PreDispatchRuntime>,
    dispatch_options: &petri_domain::DispatchOptions,
) -> Result<PersistedEvent, ServiceError> {
    let transition_id = &transition.id;
    let handler_id = transition.effect_handler_id.as_ref().unwrap();

    let input_arcs = net.input_arcs(transition_id);
    let binding = find_valid_binding(executor, transition, &input_arcs, marking, schema_registry)
        .ok_or_else(|| ServiceError::GuardNotSatisfied(transition_id.clone()))?;

    let mode = *execution_mode.read().unwrap();

    match mode {
        ExecutionMode::Live => {
            // Look up handler (drop guard before .await)
            let handler = {
                let handlers = effect_handlers.read().unwrap();
                handlers.get(handler_id).cloned().ok_or_else(|| {
                    ServiceError::Internal(format!("Effect handler not found: {}", handler_id))
                })?
            };

            // Sub-phase 2.5e-γ.mekhan: apply per-run stage_overrides
            // (RFC 7396 JSON merge-patch) BEFORE secret resolution + the
            // pre-dispatch hook chain. Overrides keyed by transition_id.0;
            // unknown IDs fail closed at scenario-load time, so a present
            // override here is guaranteed to target a declared transition.
            // No-op when there is no override entry for this transition.
            let patched_effect_config =
                apply_stage_override(&transition.effect_config, dispatch_options, &transition_id.0);

            // Resolve secrets in config (just-in-time, transient copy)
            let resolved_config = match (secret_store, &patched_effect_config) {
                (Some(store), Some(config)) => Some(
                    aithericon_secrets::resolve_secrets(config, store)
                        .await
                        .map_err(|e| ServiceError::SecretResolutionFailed {
                            transition_id: transition_id.clone(),
                            message: e.to_string(),
                        })?,
                ),
                (_, config) => config.clone(),
            };

            // Build effect input with resolved config
            // Extract read_inputs: entries from port_inputs that came via read arcs
            let read_inputs = collect_read_inputs(&binding);
            let process_step = select_process_step(transition);

            // ============================================================
            // Pre-dispatch hook chain (spec § 2-9). Live mode only.
            // ============================================================
            let mut final_config = resolved_config.clone();
            if let Some(rt) = pre_dispatch {
                if !rt.chain.is_empty() {
                    let metadata_template = PreDispatchMetadata {
                        scenario_id: None,
                        // Generic infra: submitter-supplied `net_parameters.tenant_id`
                        // (set on the spawned net at scenario-load) populates the
                        // pre-dispatch metadata's `tenant_id`, which threads onward
                        // into `HttpPreDispatchRequest.metadata.tenant_id`. The
                        // engine ascribes no semantics to the value — it is an opaque
                        // string the net's parameter bag declares.
                        tenant_id: net_parameters
                            .and_then(|p| p.get("tenant_id"))
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        correlation_id: None,
                        process_step: process_step.clone(),
                        hook_chain_index: 0,
                    };
                    let chain_inputs = ChainEvalInputs {
                        net_id: rt.net_id.as_str(),
                        transition_id,
                        transition_name: transition.name.as_str(),
                        effect_handler_id: Some(handler_id.as_str()),
                        inputs: &binding.port_inputs,
                        read_inputs: &read_inputs,
                        effect_config: final_config.as_ref(),
                        net_parameters,
                        metadata_template,
                    };
                    let (outcome, trace) = evaluate_chain(&rt.chain, &chain_inputs).await;

                    // Determine the kind for the PreDispatchEvaluated event.
                    let final_kind = match &outcome {
                        ChainEvalOutcome::Continue { .. } => {
                            petri_domain::PreDispatchOutcomeKind::Continue
                        }
                        ChainEvalOutcome::Reject { .. } => {
                            petri_domain::PreDispatchOutcomeKind::Reject
                        }
                        ChainEvalOutcome::Defer { .. } => {
                            petri_domain::PreDispatchOutcomeKind::Defer
                        }
                    };

                    // Always emit PreDispatchEvaluated.
                    let _ = events
                        .append(DomainEvent::PreDispatchEvaluated {
                            transition_id: transition_id.clone(),
                            transition_name: Some(transition.name.clone()),
                            hook_chain: trace,
                            final_outcome: final_kind,
                            timestamp: chrono::Utc::now(),
                        })
                        .await?;

                    match outcome {
                        ChainEvalOutcome::Continue {
                            enriched_effect_config,
                        } => {
                            rt.budgets.reset(&rt.net_id, transition_id);
                            if let Some(enriched) = enriched_effect_config {
                                final_config = Some(enriched);
                            }
                        }
                        ChainEvalOutcome::Reject { hook_name, reason } => {
                            let _ = events
                                .append(DomainEvent::PreDispatchRejected {
                                    transition_id: transition_id.clone(),
                                    hook_name: hook_name.clone(),
                                    reason: reason.clone(),
                                    timestamp: chrono::Utc::now(),
                                })
                                .await?;
                            rt.budgets.reset(&rt.net_id, transition_id);
                            return Err(ServiceError::PreDispatchRejected {
                                transition_id: transition_id.clone(),
                                hook_name,
                                reason,
                            });
                        }
                        ChainEvalOutcome::Defer {
                            hook_name,
                            retry_after,
                        } => {
                            let count = rt.budgets.bump(&rt.net_id, transition_id);
                            let _ = events
                                .append(DomainEvent::PreDispatchDeferred {
                                    transition_id: transition_id.clone(),
                                    hook_name: hook_name.clone(),
                                    retry_after_ms: retry_after.as_millis() as u64,
                                    defer_count: count,
                                    timestamp: chrono::Utc::now(),
                                })
                                .await?;

                            if count > rt.budgets.max_defers() {
                                // Escalate to Reject with synthetic reason.
                                let reason = "defer-budget-exceeded".to_string();
                                let _ = events
                                    .append(DomainEvent::PreDispatchRejected {
                                        transition_id: transition_id.clone(),
                                        hook_name: hook_name.clone(),
                                        reason: reason.clone(),
                                        timestamp: chrono::Utc::now(),
                                    })
                                    .await?;
                                rt.budgets.reset(&rt.net_id, transition_id);
                                return Err(ServiceError::PreDispatchRejected {
                                    transition_id: transition_id.clone(),
                                    hook_name,
                                    reason,
                                });
                            }

                            return Err(ServiceError::PreDispatchDeferred {
                                transition_id: transition_id.clone(),
                                hook_name,
                                retry_after_ms: retry_after.as_millis() as u64,
                            });
                        }
                    }
                }
            }
            // ============================================================
            // End pre-dispatch hook chain.
            // ============================================================

            let effect_input = EffectInput {
                transition_id: transition_id.clone(),
                inputs: binding.port_inputs.clone(),
                config: final_config,
                read_inputs,
                process_step,
            };

            // Execute handler
            let effect_result = handler.execute(effect_input).await;

            match effect_result {
                Ok(effect_output) => {
                    // Success path — route output tokens from effect result
                    match route_output_tokens(
                        net,
                        transition,
                        transition_id,
                        effect_output.tokens,
                        &binding.consumed_reply_routing,
                        schema_registry,
                        net_parameters,
                        Some(&effect_output.result),
                    ) {
                        Ok((produced_tokens, bridge_out_tokens)) => {
                            let event = events
                                .append(DomainEvent::EffectCompleted {
                                    transition_id: transition_id.clone(),
                                    transition_name: Some(transition.name.clone()),
                                    consumed_tokens: binding.consumed_tokens,
                                    produced_tokens,
                                    effect_handler_id: handler_id.clone(),
                                    effect_result: effect_output.result,
                                    read_tokens: binding.read_tokens,
                                    process_step_started: transition.process_step_started.clone(),
                                    process_step_completed: transition
                                        .process_step_completed
                                        .clone(),
                                })
                                .await?;

                            emit_bridge_out_events(
                                events,
                                transition_id,
                                bridge_out_tokens,
                                Some(event.sequence),
                            )
                            .await?;

                            Ok(event)
                        }
                        Err(routing_err) => {
                            // The handler's side effect already ran, but output
                            // routing failed (e.g. an output token violates its
                            // declared schema). This is permanent — re-running
                            // would produce the same bad output. Record an
                            // `EffectFailed` (NOT `ErrorOccurred`: the replay
                            // cursor only tracks Effect* events, so a non-effect
                            // event here would desync replay) and consume the
                            // input tokens so the transition can't be
                            // re-selected forever.
                            events
                                .append(DomainEvent::EffectFailed {
                                    transition_id: transition_id.clone(),
                                    transition_name: Some(transition.name.clone()),
                                    consumed_tokens: binding.consumed_tokens.clone(),
                                    produced_tokens: vec![],
                                    effect_handler_id: handler_id.clone(),
                                    error_message: routing_err.to_string(),
                                    tokens_consumed: true,
                                    input_data: Some(binding.port_inputs.clone()),
                                    retryable: false,
                                })
                                .await?;

                            Err(routing_err)
                        }
                    }
                }
                Err(effect_error) => {
                    let error_message = effect_error.to_string();
                    let retryable = effect_error.is_retryable();

                    if transition.output_port("_error").is_some() {
                        // Error port path: consume tokens, route error token to _error place
                        let error_data = serde_json::json!({
                            "error": error_message,
                            "handler_id": handler_id,
                            "transition_id": transition_id.to_string(),
                            "inputs": binding.port_inputs,
                            "retryable": retryable,
                        });
                        let mut error_output = HashMap::new();
                        error_output.insert("_error".to_string(), error_data);

                        let (produced_tokens, bridge_out_tokens) = route_output_tokens(
                            net,
                            transition,
                            transition_id,
                            error_output,
                            &binding.consumed_reply_routing,
                            None, // Skip schema validation for error tokens
                            net_parameters,
                            None, // No effect result on error path
                        )?;

                        let event = events
                            .append(DomainEvent::EffectFailed {
                                transition_id: transition_id.clone(),
                                transition_name: Some(transition.name.clone()),
                                consumed_tokens: binding.consumed_tokens,
                                produced_tokens,
                                effect_handler_id: handler_id.clone(),
                                error_message,
                                tokens_consumed: true,
                                input_data: Some(binding.port_inputs.clone()),
                                retryable,
                            })
                            .await?;

                        emit_bridge_out_events(
                            events,
                            transition_id,
                            bridge_out_tokens,
                            Some(event.sequence),
                        )
                        .await?;
                        Ok(event)
                    } else {
                        // No `_error` port: the transition cannot make progress
                        // on a retry, so consume the input tokens (marking
                        // advances → transition is no longer selected → the
                        // eval loop reaches genuine quiescence instead of
                        // re-firing forever). The full input is preserved in
                        // the audit event. Retry/compensation is authored via
                        // an `_error` port; `retryable` is recorded for
                        // observability only.
                        events
                            .append(DomainEvent::EffectFailed {
                                transition_id: transition_id.clone(),
                                transition_name: Some(transition.name.clone()),
                                consumed_tokens: binding.consumed_tokens.clone(),
                                produced_tokens: vec![],
                                effect_handler_id: handler_id.clone(),
                                error_message: error_message.clone(),
                                tokens_consumed: true,
                                input_data: Some(binding.port_inputs.clone()),
                                retryable,
                            })
                            .await?;

                        Err(ServiceError::EffectFailed {
                            transition_id: transition_id.clone(),
                            handler_id: handler_id.clone(),
                            message: error_message,
                            retryable,
                        })
                    }
                }
            }
        }
        ExecutionMode::Replay => {
            let all_events = events.all_events().await;
            let cursor = *replay_cursor.read().unwrap();

            // Collect ALL effect events (completed + failed) in log order
            let effect_events: Vec<_> = all_events
                .iter()
                .filter(|e| {
                    matches!(
                        &e.event,
                        DomainEvent::EffectCompleted { .. } | DomainEvent::EffectFailed { .. }
                    )
                })
                .collect();

            let stored = effect_events.get(cursor).ok_or_else(|| {
                ServiceError::Internal(format!(
                    "Replay: no effect event at cursor {} (total: {})",
                    cursor,
                    effect_events.len()
                ))
            })?;

            match &stored.event {
                DomainEvent::EffectCompleted {
                    transition_id: tid,
                    effect_handler_id: hid,
                    effect_result,
                    produced_tokens,
                    consumed_tokens,
                    read_tokens: stored_read_tokens,
                    ..
                } if *tid == *transition_id && *hid == *handler_id => {
                    *replay_cursor.write().unwrap() = cursor + 1;

                    // Replay handler (drop guard before .await)
                    {
                        let handlers = effect_handlers.read().unwrap();
                        if let Some(handler) = handlers.get(handler_id) {
                            let effect_input = EffectInput {
                                transition_id: transition_id.clone(),
                                inputs: binding.port_inputs.clone(),
                                config: transition.effect_config.clone(),
                                read_inputs: collect_read_inputs(&binding),
                                process_step: select_process_step(transition),
                            };
                            handler.replay(&effect_input, effect_result);
                        }
                    }

                    // Re-emit the stored event (same tokens, same result)
                    let event = events
                        .append(DomainEvent::EffectCompleted {
                            transition_id: transition_id.clone(),
                            transition_name: Some(transition.name.clone()),
                            consumed_tokens: consumed_tokens.clone(),
                            produced_tokens: produced_tokens.clone(),
                            effect_handler_id: handler_id.clone(),
                            effect_result: effect_result.clone(),
                            read_tokens: stored_read_tokens.clone(),
                            process_step_started: transition.process_step_started.clone(),
                            process_step_completed: transition.process_step_completed.clone(),
                        })
                        .await?;

                    Ok(event)
                }
                DomainEvent::EffectFailed {
                    transition_id: tid,
                    effect_handler_id: hid,
                    consumed_tokens,
                    produced_tokens,
                    error_message,
                    tokens_consumed,
                    input_data,
                    retryable,
                    ..
                } if *tid == *transition_id && *hid == *handler_id => {
                    *replay_cursor.write().unwrap() = cursor + 1;

                    // Re-emit the stored EffectFailed event
                    let event = events
                        .append(DomainEvent::EffectFailed {
                            transition_id: transition_id.clone(),
                            transition_name: Some(transition.name.clone()),
                            consumed_tokens: consumed_tokens.clone(),
                            produced_tokens: produced_tokens.clone(),
                            effect_handler_id: handler_id.clone(),
                            error_message: error_message.clone(),
                            tokens_consumed: *tokens_consumed,
                            input_data: input_data.clone(),
                            retryable: *retryable,
                        })
                        .await?;

                    if *tokens_consumed {
                        Ok(event)
                    } else {
                        Err(ServiceError::EffectFailed {
                            transition_id: transition_id.clone(),
                            handler_id: handler_id.clone(),
                            message: error_message.clone(),
                            retryable: *retryable,
                        })
                    }
                }
                _ => Err(ServiceError::Internal(format!(
                    "Replay mismatch at cursor {}: expected ({}, {})",
                    cursor, transition_id, handler_id
                ))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_param_literal_string() {
        let result = resolve_param("my-net-123", None, None).unwrap();
        assert_eq!(result, "my-net-123");
    }

    #[test]
    fn test_resolve_param_literal_with_params_present() {
        let params = serde_json::json!({"parent": "net-a"});
        let result = resolve_param("static-net", Some(&params), None).unwrap();
        assert_eq!(result, "static-net");
    }

    #[test]
    fn test_resolve_param_from_params() {
        let params = serde_json::json!({"parent_net_id": "orchestrator-abc"});
        let result = resolve_param("$params.parent_net_id", Some(&params), None).unwrap();
        assert_eq!(result, "orchestrator-abc");
    }

    #[test]
    fn test_resolve_param_missing_params() {
        let result = resolve_param("$params.parent_net_id", None, None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("net has no parameters"));
    }

    #[test]
    fn test_resolve_param_missing_key() {
        let params = serde_json::json!({"other_key": "value"});
        let result = resolve_param("$params.parent_net_id", Some(&params), None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not found or not a string"));
    }

    #[test]
    fn test_resolve_param_non_string_value() {
        let params = serde_json::json!({"parent_net_id": 42});
        let result = resolve_param("$params.parent_net_id", Some(&params), None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not found or not a string"));
    }

    #[test]
    fn test_resolve_param_from_result() {
        let effect_result = serde_json::json!({"child_net_id": "spawned-net-xyz"});
        let result = resolve_param("$result.child_net_id", None, Some(&effect_result)).unwrap();
        assert_eq!(result, "spawned-net-xyz");
    }

    #[test]
    fn test_resolve_param_result_missing() {
        let result = resolve_param("$result.child_net_id", None, None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no effect result available"));
    }

    #[test]
    fn test_resolve_param_result_key_missing() {
        let effect_result = serde_json::json!({"other_key": "value"});
        let result = resolve_param("$result.child_net_id", None, Some(&effect_result));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not found or not a string"));
    }

    // ── SCATTER: Batch-cardinality output ports ──────────────────────────

    use petri_domain::{Arc as PetriArc, PetriNet, Place, Port, PortCardinality};
    use std::collections::HashMap as StdHashMap;

    use crate::rhai_runtime::token_color_to_json;
    use crate::schema_registry::SchemaRegistry;

    /// Build a single-transition net with one output port wired to one place.
    /// `cardinality` controls scatter behavior; `schema_ref` is optional.
    fn scatter_net(
        port_name: &str,
        cardinality: PortCardinality,
        schema_ref: Option<&str>,
    ) -> (PetriNet, Transition, TransitionId) {
        let mut net = PetriNet::new();

        let place = Place::internal("out_place");
        let place_id = net.add_place(place);

        let mut port = Port::new(port_name).with_cardinality(cardinality);
        if let Some(s) = schema_ref {
            port = port.with_schema(s);
        }
        let mut transition = Transition::new("scatterer", r#"#{}"#);
        transition.output_ports = vec![port];
        let transition_id = net.add_transition(transition.clone());

        net.add_arc(PetriArc::output(transition_id.clone(), port_name, place_id));

        (net, transition, transition_id)
    }

    #[test]
    fn test_scatter_batch_output_emits_one_token_per_element_in_order() {
        let (net, transition, transition_id) = scatter_net("items", PortCardinality::Batch, None);

        let mut script_result: StdHashMap<String, JsonValue> = StdHashMap::new();
        script_result.insert(
            "items".to_string(),
            serde_json::json!([{"i": 0}, {"i": 1}, {"i": 2}]),
        );

        let (produced, bridge_out) = route_output_tokens(
            &net,
            &transition,
            &transition_id,
            script_result,
            &None,
            None,
            None,
            None,
        )
        .expect("scatter should succeed");

        assert!(bridge_out.is_empty());
        assert_eq!(produced.len(), 3, "3 array elements → 3 tokens");

        // Array order preserved, each token carries its element.
        for (idx, (_place_id, token)) in produced.iter().enumerate() {
            let data = token_color_to_json(&token.color);
            assert_eq!(data, serde_json::json!({"i": idx as i64}));
        }
    }

    #[test]
    fn test_scatter_batch_per_element_schema_validation() {
        // schema_ref demands `i` be an integer; one element violates it.
        let mut defs: StdHashMap<String, JsonValue> = StdHashMap::new();
        defs.insert(
            "Item".to_string(),
            serde_json::json!({
                "type": "object",
                "properties": { "i": { "type": "integer" } },
                "required": ["i"]
            }),
        );
        let registry = SchemaRegistry::new(defs).expect("registry builds");

        let (net, transition, transition_id) =
            scatter_net("items", PortCardinality::Batch, Some("Item"));

        let mut script_result: StdHashMap<String, JsonValue> = StdHashMap::new();
        script_result.insert(
            "items".to_string(),
            serde_json::json!([{"i": 0}, {"i": "not-an-int"}, {"i": 2}]),
        );

        let err = route_output_tokens(
            &net,
            &transition,
            &transition_id,
            script_result,
            &None,
            Some(&registry),
            None,
            None,
        )
        .expect_err("element violating schema_ref must fail");

        assert!(
            matches!(err, ServiceError::SchemaValidationFailed { .. }),
            "expected SchemaValidationFailed, got {:?}",
            err
        );
    }

    #[test]
    fn test_scatter_batch_non_array_is_permanent_error() {
        let (net, transition, transition_id) = scatter_net("items", PortCardinality::Batch, None);

        let mut script_result: StdHashMap<String, JsonValue> = StdHashMap::new();
        // Object, not an array, on a Batch output port.
        script_result.insert("items".to_string(), serde_json::json!({"not": "array"}));

        let err = route_output_tokens(
            &net,
            &transition,
            &transition_id,
            script_result,
            &None,
            None,
            None,
            None,
        )
        .expect_err("non-array on Batch output must fail");

        assert!(err.is_permanent(), "BatchOutputNotArray must be permanent");
        match err {
            ServiceError::BatchOutputNotArray { port_name } => {
                assert_eq!(port_name, "items");
            }
            other => panic!("expected BatchOutputNotArray, got {:?}", other),
        }
    }

    #[test]
    fn test_scatter_single_output_yields_exactly_one_token() {
        // Regression: a Single (default) output port keeps the whole value in
        // ONE token, even when that value is itself an array.
        let (net, transition, transition_id) = scatter_net("result", PortCardinality::Single, None);

        let mut script_result: StdHashMap<String, JsonValue> = StdHashMap::new();
        let whole = serde_json::json!([1, 2, 3]);
        script_result.insert("result".to_string(), whole.clone());

        let (produced, bridge_out) = route_output_tokens(
            &net,
            &transition,
            &transition_id,
            script_result,
            &None,
            None,
            None,
            None,
        )
        .expect("single output should succeed");

        assert!(bridge_out.is_empty());
        assert_eq!(produced.len(), 1, "Single port → exactly one token");
        assert_eq!(token_color_to_json(&produced[0].1.color), whole);
    }
}

// =============================================================================
// Sub-phase 2.5e-γ.mekhan skip path + stage_overrides merge-patch helper
// =============================================================================

/// Sub-phase 2.5e-γ.mekhan: skip-path executor. Consumes input tokens via
/// the standard binding-selection path, then emits a `TransitionSkipped`
/// domain event with `Token::new_unit()` on each output arc's target
/// place. NO effect dispatch, NO Rhai logic, NO pre-dispatch hook chain.
///
/// Iterates `net.output_arcs(&transition_id)` (the structural source of
/// truth for "declared output ports that resolve to a place"): each arc
/// produces exactly one Unit token at its target place. Output ports
/// without arcs are silently ignored — they have no place to write to —
/// which matches the engine's existing route_output_tokens behaviour
/// where an arc is required for routing.
///
/// Binding selection reuses `find_valid_binding` with the same arguments
/// the Rhai / Effect branches use; the skip event docstring guarantees
/// that consumed_tokens come from a regularly-enabled binding (skip
/// happens AFTER input-binding selection per the spec).
async fn execute_skip<E: EventRepository>(
    events: &E,
    executor: &TransitionExecutor,
    net: &PetriNet,
    marking: &petri_domain::Marking,
    transition: &petri_domain::Transition,
    transition_id: &TransitionId,
    schema_registry: Option<&SchemaRegistry>,
) -> Result<PersistedEvent, ServiceError> {
    let input_arcs = net.input_arcs(transition_id);
    let binding = find_valid_binding(executor, transition, &input_arcs, marking, schema_registry)
        .ok_or_else(|| ServiceError::GuardNotSatisfied(transition_id.clone()))?;

    // Produce one `Token::new_unit()` per declared output arc. The arc set
    // is the structural ground truth — output ports without arcs would have
    // no destination place anyway (mirrors route_output_tokens' arc-lookup
    // failure mode for the live path).
    let produced_tokens: Vec<(PlaceId, Token)> = net
        .output_arcs(transition_id)
        .into_iter()
        .map(|arc| (arc.place_id.clone(), Token::new_unit()))
        .collect();

    let event = events
        .append(DomainEvent::TransitionSkipped {
            transition_id: transition_id.clone(),
            transition_name: Some(transition.name.clone()),
            consumed_tokens: binding.consumed_tokens,
            produced_tokens,
            skip_reason: "skip_mask".to_string(),
        })
        .await?;

    Ok(event)
}

/// Sub-phase 2.5e-γ.mekhan: apply a per-transition `stage_overrides`
/// merge-patch to the transition's static `effect_config` BEFORE secret
/// resolution + pre-dispatch hook chain enrichment.
///
/// Semantics:
/// - No override for this transition_id → returns the original config clone.
/// - Override present, original `effect_config` is `None` → patch applies to
///   an empty JSON object (`{}`), producing a new config from the patch.
/// - Override present, original `effect_config` is `Some(json)` → RFC 7396
///   merge-patch applied to the cloned base, returning the merged config.
///
/// Failing-closed on unknown transition_id is the scenario-load layer's
/// responsibility (cloud-layer-side per the dispatch contract); by the
/// time we reach this helper, any present override targets a declared
/// transition. We intentionally do NOT validate per-step model presence
/// here — per `feedback_no_default_model`, the upstream submit-path is
/// the guard surface; mekhan's role is purely to apply the patch.
fn apply_stage_override(
    base: &Option<JsonValue>,
    dispatch_options: &petri_domain::DispatchOptions,
    transition_id_str: &str,
) -> Option<JsonValue> {
    let patch = match dispatch_options.stage_overrides.get(transition_id_str) {
        Some(p) => p,
        None => return base.clone(),
    };
    let mut merged = base
        .clone()
        .unwrap_or_else(|| JsonValue::Object(serde_json::Map::new()));
    petri_domain::apply_merge_patch(&mut merged, patch);
    Some(merged)
}

#[cfg(test)]
mod skip_and_override_tests {
    use super::*;
    use petri_domain::DispatchOptions;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn apply_stage_override_no_entry_returns_original_some() {
        let base = Some(json!({"temperature": 0.7, "model": "qwen3.5:9b"}));
        let opts = DispatchOptions::default();
        let result = apply_stage_override(&base, &opts, "agent_a");
        assert_eq!(result, base);
    }

    #[test]
    fn apply_stage_override_no_entry_returns_original_none() {
        let base: Option<JsonValue> = None;
        let opts = DispatchOptions::default();
        let result = apply_stage_override(&base, &opts, "agent_a");
        assert_eq!(result, None);
    }

    #[test]
    fn apply_stage_override_merge_temperature_overrides_at_fire_time() {
        // Spec: a stage_override with `{"temperature": 0.0}` produces an
        // effect_config with temperature 0.0 overriding the original at
        // fire-time. This is the canonical example from the bootstrap brief.
        let base = Some(json!({
            "model": "qwen3.5:9b",
            "temperature": 0.7,
            "tools": ["search", "code"],
        }));
        let mut stage_overrides = HashMap::new();
        stage_overrides.insert("agent_a".to_string(), json!({"temperature": 0.0}));
        let opts = DispatchOptions {
            skip_mask: vec![],
            stage_overrides,
        };
        let result = apply_stage_override(&base, &opts, "agent_a").unwrap();
        assert_eq!(
            result,
            json!({
                "model": "qwen3.5:9b",
                "temperature": 0.0,
                "tools": ["search", "code"],
            }),
            "merge-patch must override leaf temperature while preserving siblings"
        );
    }

    #[test]
    fn apply_stage_override_nested_path_preserves_unrelated_branches() {
        // Sanity-check that the per-transition path layers cleanly on top
        // of the underlying RFC 7396 merge-patch primitive (which already
        // has 7 unit tests in petri_domain::dispatch).
        let base = Some(json!({
            "model_config": {
                "model": "qwen3.5:9b",
                "temperature": 0.7,
            },
            "retry": {"max_attempts": 3},
        }));
        let mut stage_overrides = HashMap::new();
        stage_overrides.insert(
            "step_x".to_string(),
            json!({"model_config": {"temperature": 0.0}}),
        );
        let opts = DispatchOptions {
            skip_mask: vec![],
            stage_overrides,
        };
        let result = apply_stage_override(&base, &opts, "step_x").unwrap();
        assert_eq!(
            result,
            json!({
                "model_config": {
                    "model": "qwen3.5:9b",
                    "temperature": 0.0,
                },
                "retry": {"max_attempts": 3},
            })
        );
    }

    #[test]
    fn apply_stage_override_targets_only_matching_transition_id() {
        // An override keyed by "other_step" must not affect "agent_a".
        let base = Some(json!({"temperature": 0.7}));
        let mut stage_overrides = HashMap::new();
        stage_overrides.insert("other_step".to_string(), json!({"temperature": 0.0}));
        let opts = DispatchOptions {
            skip_mask: vec![],
            stage_overrides,
        };
        let result = apply_stage_override(&base, &opts, "agent_a");
        assert_eq!(result, base);
    }

    #[test]
    fn apply_stage_override_with_none_base_creates_config_from_patch() {
        // If a transition has no static effect_config and an override is
        // submitted for it, the patch builds the config from {}. This is
        // an honest semantics call: the merge-patch primitive on `{}` +
        // `{"x": 1}` returns `{"x": 1}`, so the resulting effect_config is
        // the patch itself when there is no base. Upstream submit-path
        // validation owns the "is this safe?" decision (e.g. model-presence
        // per `feedback_no_default_model`).
        let base: Option<JsonValue> = None;
        let mut stage_overrides = HashMap::new();
        stage_overrides.insert("agent_a".to_string(), json!({"temperature": 0.0}));
        let opts = DispatchOptions {
            skip_mask: vec![],
            stage_overrides,
        };
        let result = apply_stage_override(&base, &opts, "agent_a").unwrap();
        assert_eq!(result, json!({"temperature": 0.0}));
    }

    #[test]
    fn apply_stage_override_null_value_deletes_key() {
        // RFC 7396: null in the patch deletes the key. Verify the per-
        // transition wrapper exposes this primitive correctly.
        let base = Some(json!({"temperature": 0.7, "max_tokens": 512}));
        let mut stage_overrides = HashMap::new();
        stage_overrides.insert("agent_a".to_string(), json!({"max_tokens": null}));
        let opts = DispatchOptions {
            skip_mask: vec![],
            stage_overrides,
        };
        let result = apply_stage_override(&base, &opts, "agent_a").unwrap();
        assert_eq!(result, json!({"temperature": 0.7}));
    }
}
