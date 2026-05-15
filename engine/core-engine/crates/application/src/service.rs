use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use petri_domain::{
    DomainEvent, Marking, PersistedEvent, PetriNet, PlaceId, ReplyRouting, Token, TokenColor,
    TokenId, TransitionId,
};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use aithericon_secrets::SecretStore;

use crate::evaluation::{self, get_marking_cached, EvaluateResult, TransitionStatusDetail};
use crate::firing;
use crate::pre_dispatch::PreDispatchRuntime;
use crate::schema_registry::SchemaRegistry;
use crate::token_manager;
use crate::{
    effect::{EffectHandler, ExecutionMode},
    rhai_runtime::json_to_token_color,
    EventRepository, ServiceError, StateProjection, TopologyRepository, TransitionExecutor,
};

/// Configuration controlling which validation checks are active.
#[derive(Clone, Debug)]
pub struct ExecutionConfig {
    /// Validate output tokens against port schema_ref after script/effect execution.
    pub validate_output_schemas: bool,
    /// Validate injected tokens against place token_schema.
    pub validate_injection_schemas: bool,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            validate_output_schemas: true,
            validate_injection_schemas: true,
        }
    }
}

/// The main application service for the Petri Net engine.
/// Handles commands and orchestrates domain logic.
pub struct PetriNetService<E, T, S>
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    events: Arc<E>,
    topology: Arc<T>,
    projection: Arc<S>,
    executor: TransitionExecutor,
    /// Initial tokens to restore on reset
    initial_tokens: RwLock<Vec<(PlaceId, TokenColor)>>,
    /// Workflow ID for this service instance.
    workflow_id: RwLock<Option<Uuid>>,
    /// Execution mode (Live or Replay).
    execution_mode: RwLock<ExecutionMode>,
    /// Effect handlers keyed by handler ID.
    effect_handlers: RwLock<HashMap<String, Arc<dyn EffectHandler>>>,
    /// Cached marking state: (sequence_number, marking).
    cached_state: RwLock<Option<(u64, Marking)>>,
    /// Monotonic cursor into the effect event log for deterministic replay.
    replay_cursor: RwLock<usize>,
    /// Schema registry for token data validation.
    schema_registry: RwLock<Option<Arc<SchemaRegistry>>>,
    /// Execution configuration controlling validation behavior.
    execution_config: RwLock<crate::service::ExecutionConfig>,
    /// Secret store for resolving `{{secret:KEY}}` patterns in effect configs.
    secret_store: RwLock<Option<Arc<dyn SecretStore>>>,
    /// Net parameters (from CreateNetRequest), accessible at runtime for `$params.` resolution.
    net_parameters: RwLock<Option<serde_json::Value>>,
    /// Evaluation lock: prevents concurrent `evaluate_until_quiescent` calls
    /// from racing on the same marking state. The eval loop and HTTP handler
    /// both call evaluate; without this, both can read the same marking and
    /// fire the same effect transition on the same token.
    eval_lock: tokio::sync::Mutex<()>,
    /// Engine-level idempotency for `TokenCreated` events with a `dedup_id`.
    /// Catches listener-message redeliveries that land after the JetStream
    /// 120s dedup window expires. Lazy-seeded from the durable event log.
    dedup_index: crate::idempotency_index::DedupIndex,
    /// Pre-dispatch hook runtime (chain + defer budgets). `None` until the
    /// owning NetRegistry binds the chain at net-instantiation time. See
    /// `pre-dispatch-hook.md` § 6 for the registration model.
    pre_dispatch: RwLock<Option<Arc<PreDispatchRuntime>>>,
}

impl<E, T, S> PetriNetService<E, T, S>
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    pub fn new(events: Arc<E>, topology: Arc<T>, projection: Arc<S>) -> Self {
        Self {
            events,
            topology,
            projection,
            executor: TransitionExecutor::new(),
            initial_tokens: RwLock::new(Vec::new()),
            workflow_id: RwLock::new(None),
            execution_mode: RwLock::new(ExecutionMode::Live),
            effect_handlers: RwLock::new(HashMap::new()),
            cached_state: RwLock::new(None),
            replay_cursor: RwLock::new(0),
            schema_registry: RwLock::new(None),
            execution_config: RwLock::new(ExecutionConfig::default()),
            secret_store: RwLock::new(None),
            net_parameters: RwLock::new(None),
            eval_lock: tokio::sync::Mutex::new(()),
            dedup_index: crate::idempotency_index::DedupIndex::new(),
            pre_dispatch: RwLock::new(None),
        }
    }

    /// Bind the pre-dispatch hook runtime (chain + defer budgets) to this
    /// service. Called once by the `NetRegistry` at net-instantiation time
    /// after the registry's hook chain is resolved from the TOML config +
    /// builtin map. Re-binding silently overwrites — the registry is the
    /// source of truth, and this method is `&self` to mirror the rest of
    /// the service's interior-mutability setters.
    pub fn set_pre_dispatch_runtime(&self, rt: Arc<PreDispatchRuntime>) {
        *self.pre_dispatch.write().unwrap() = Some(rt);
    }

    /// Read-only access to the bound runtime, if any.
    pub fn pre_dispatch_runtime(&self) -> Option<Arc<PreDispatchRuntime>> {
        self.pre_dispatch.read().unwrap().clone()
    }

    // ========================================================================
    // Configuration
    // ========================================================================

    /// Set the execution mode.
    /// Resets the replay cursor when switching to Replay mode.
    pub fn set_execution_mode(&self, mode: ExecutionMode) {
        *self.execution_mode.write().unwrap() = mode;
        if mode == ExecutionMode::Replay {
            *self.replay_cursor.write().unwrap() = 0;
        }
    }

    /// Get the current execution mode.
    pub fn execution_mode(&self) -> ExecutionMode {
        *self.execution_mode.read().unwrap()
    }

    /// Set net parameters (from CreateNetRequest) for `$params.` resolution in bridge targets.
    pub fn set_net_parameters(&self, params: serde_json::Value) {
        *self.net_parameters.write().unwrap() = Some(params);
    }

    /// Get the current net parameters.
    pub fn net_parameters(&self) -> Option<serde_json::Value> {
        self.net_parameters.read().unwrap().clone()
    }

    /// Register an effect handler under the given ID.
    ///
    /// If the handler declares `port_schemas()` and a topology is loaded,
    /// validates that the handler's port contract matches the transition
    /// definitions. Returns an error on mismatch.
    pub fn register_effect_handler(
        &self,
        id: impl Into<String>,
        handler: Arc<dyn EffectHandler>,
    ) -> Result<(), ServiceError> {
        let id = id.into();

        // Contract validation: if handler declares port schemas and topology is loaded
        if let Some(schemas) = handler.port_schemas() {
            if let Some(net) = self.topology.get_topology() {
                // Find all transitions that reference this handler
                for transition in net.transitions.values() {
                    if transition.effect_handler_id.as_deref() != Some(&id) {
                        continue;
                    }

                    // Validate input port contracts
                    for (port_name, expected_schema) in &schemas.inputs {
                        match transition.input_port(port_name) {
                            None => {
                                return Err(ServiceError::EffectContractError(format!(
                                    "Handler '{}' declares input port '{}' but transition '{}' has no such port",
                                    id, port_name, transition.name
                                )));
                            }
                            Some(port) => {
                                if let Some(ref actual_schema) = port.schema_ref {
                                    // DynamicToken is the "any type" escape hatch —
                                    // always compatible with handler-declared schemas.
                                    if actual_schema != expected_schema
                                        && actual_schema != "#/definitions/DynamicToken"
                                    {
                                        return Err(ServiceError::EffectContractError(format!(
                                            "Handler '{}' expects schema '{}' on input port '{}' but transition '{}' declares '{}'",
                                            id, expected_schema, port_name, transition.name, actual_schema
                                        )));
                                    }
                                }
                            }
                        }
                    }

                    // Validate output port contracts
                    for (port_name, expected_schema) in &schemas.outputs {
                        match transition.output_port(port_name) {
                            None => {
                                return Err(ServiceError::EffectContractError(format!(
                                    "Handler '{}' declares output port '{}' but transition '{}' has no such port",
                                    id, port_name, transition.name
                                )));
                            }
                            Some(port) => {
                                if let Some(ref actual_schema) = port.schema_ref {
                                    if actual_schema != expected_schema
                                        && actual_schema != "#/definitions/DynamicToken"
                                    {
                                        return Err(ServiceError::EffectContractError(format!(
                                            "Handler '{}' expects schema '{}' on output port '{}' but transition '{}' declares '{}'",
                                            id, expected_schema, port_name, transition.name, actual_schema
                                        )));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        self.effect_handlers.write().unwrap().insert(id, handler);
        Ok(())
    }

    /// Returns the IDs of all currently registered effect handlers.
    pub fn registered_handler_ids(&self) -> Vec<String> {
        self.effect_handlers
            .read()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    /// Set the workflow ID for this service instance.
    pub fn set_workflow_id(&self, workflow_id: Uuid) {
        *self.workflow_id.write().unwrap() = Some(workflow_id);
    }

    /// Get the current workflow ID.
    pub fn workflow_id(&self) -> Option<Uuid> {
        *self.workflow_id.read().unwrap()
    }

    /// Set initial tokens to be restored on reset.
    pub fn set_initial_tokens(&self, tokens: Vec<(PlaceId, TokenColor)>) {
        *self.initial_tokens.write().unwrap() = tokens;
    }

    /// Set the schema registry for token validation.
    pub fn set_schema_registry(&self, registry: SchemaRegistry) {
        *self.schema_registry.write().unwrap() = Some(Arc::new(registry));
    }

    /// Get the schema registry (if loaded).
    pub fn schema_registry(&self) -> Option<Arc<SchemaRegistry>> {
        self.schema_registry.read().unwrap().clone()
    }

    /// Set the execution configuration.
    pub fn set_execution_config(&self, config: ExecutionConfig) {
        *self.execution_config.write().unwrap() = config;
    }

    /// Get the execution configuration.
    pub fn execution_config(&self) -> ExecutionConfig {
        self.execution_config.read().unwrap().clone()
    }

    /// Set the secret store for resolving `{{secret:KEY}}` patterns in effect configs.
    pub fn set_secret_store(&self, store: Arc<dyn SecretStore>) {
        *self.secret_store.write().unwrap() = Some(store);
    }

    /// Get the secret store (if configured).
    fn secret_store(&self) -> Option<Arc<dyn SecretStore>> {
        self.secret_store.read().unwrap().clone()
    }

    // ========================================================================
    // Topology
    // ========================================================================

    /// Clear all events and cached state, preparing for a fresh scenario load.
    pub async fn clear(&self) {
        self.events.reset().await;
        self.invalidate_cache();
        *self.replay_cursor.write().unwrap() = 0;
        *self.schema_registry.write().unwrap() = None;
    }

    /// Initialize the net with a topology.
    pub async fn initialize(&self, net: PetriNet) -> Result<PersistedEvent, ServiceError> {
        self.topology.set_topology(net.clone());
        Ok(self
            .events
            .append(DomainEvent::NetInitialized { net })
            .await?)
    }

    /// Get the current topology.
    pub fn get_topology(&self) -> Option<PetriNet> {
        self.topology.get_topology()
    }

    /// Update a transition's script and/or guard (hot-reload).
    pub async fn update_transition_script(
        &self,
        transition_id: TransitionId,
        script: String,
        guard: Option<String>,
    ) -> Result<PersistedEvent, ServiceError> {
        self.executor.compile_check(&script)?;

        if let Some(ref guard_script) = guard {
            self.executor.compile_check(guard_script)?;
        }

        if !self
            .topology
            .update_transition_script(&transition_id, script.clone(), guard.clone())
        {
            return Err(ServiceError::TransitionNotFound(transition_id));
        }

        let event = self
            .events
            .append(DomainEvent::TransitionScriptUpdated {
                transition_id,
                script,
                guard,
            })
            .await?;

        Ok(event)
    }

    // ========================================================================
    // Token management (delegates to token_manager module)
    // ========================================================================

    /// Create a new token at a place.
    pub async fn create_token(
        &self,
        place_id: PlaceId,
        color: TokenColor,
    ) -> Result<PersistedEvent, ServiceError> {
        self.create_token_with_meta(place_id, color, None, None, None)
            .await
    }

    /// Create a new token at a place, optionally attaching reply routing context,
    /// `signal_key` for causality lineage, and `dedup_id` for engine-level
    /// idempotency. `dedup_id` should be deterministic for one-shot events
    /// (so retries collide); leave `None` for streaming events that legitimately
    /// produce many tokens at the same place under the same `signal_key`.
    pub async fn create_token_with_meta(
        &self,
        place_id: PlaceId,
        color: TokenColor,
        reply_routing: Option<ReplyRouting>,
        signal_key: Option<String>,
        dedup_id: Option<String>,
    ) -> Result<PersistedEvent, ServiceError> {
        // Engine-level idempotency: if this (place, dedup_id) was already
        // observed (in this run or replayed from history), return the existing
        // event. Catches listener redeliveries that arrive after the JetStream
        // 120s dedup window has expired. signal_key is intentionally NOT used
        // for dedup — it carries lineage and is shared across stream emits.
        if let Some(ref id) = dedup_id {
            if !id.is_empty() {
                if let Some(existing) = self
                    .dedup_index
                    .get(self.events.as_ref(), &place_id, id)
                    .await
                {
                    tracing::info!(
                        place = %place_id.0,
                        dedup_id = %id,
                        sequence = existing.sequence,
                        "Idempotent TokenCreated skipped — dedup_id already seen"
                    );
                    return Ok(existing);
                }
            }
        }

        let registry = self.schema_registry();
        let config = self.execution_config();
        let registry_ref = if config.validate_injection_schemas {
            registry.as_deref()
        } else {
            None
        };
        let event = token_manager::create_token_with_meta(
            self.events.as_ref(),
            self.topology.as_ref(),
            place_id.clone(),
            color,
            reply_routing,
            signal_key,
            dedup_id.clone(),
            registry_ref,
        )
        .await?;

        if let Some(id) = dedup_id {
            if !id.is_empty() {
                self.dedup_index
                    .insert(self.events.as_ref(), place_id, id, event.clone())
                    .await;
            }
        }

        Ok(event)
    }

    /// Remove a token from a place by token ID or correlation ID.
    pub async fn remove_token(
        &self,
        place_id: PlaceId,
        token_id: Option<TokenId>,
        correlation_id: Option<String>,
        reason: Option<String>,
    ) -> Result<PersistedEvent, ServiceError> {
        let marking = self.get_marking_cached().await;
        token_manager::remove_token(
            self.events.as_ref(),
            self.topology.as_ref(),
            &marking,
            place_id,
            token_id,
            correlation_id,
            reason,
        )
        .await
    }

    /// Update a token's data in place.
    pub async fn update_token(
        &self,
        place_id: PlaceId,
        token_id: Option<TokenId>,
        correlation_id: Option<String>,
        new_color: TokenColor,
    ) -> Result<PersistedEvent, ServiceError> {
        let marking = self.get_marking_cached().await;
        token_manager::update_token(
            self.events.as_ref(),
            self.topology.as_ref(),
            &marking,
            place_id,
            token_id,
            correlation_id,
            new_color,
        )
        .await
    }

    /// Check if a specific token exists in a specific place.
    pub async fn token_exists_in_place(&self, place_id: &PlaceId, token_id: &TokenId) -> bool {
        let marking = self.get_marking().await;
        marking
            .tokens
            .get(place_id)
            .map(|tokens| tokens.iter().any(|t| &t.id == token_id))
            .unwrap_or(false)
    }

    // ========================================================================
    // Firing (delegates to firing module)
    // ========================================================================

    /// Fire a transition using port-based routing.
    pub async fn fire_transition(
        &self,
        transition_id: TransitionId,
    ) -> Result<PersistedEvent, ServiceError> {
        let marking = self.get_marking_cached().await;
        let registry = self.schema_registry();
        let config = self.execution_config();
        let registry_ref = if config.validate_output_schemas {
            registry.as_deref()
        } else {
            None
        };
        let secrets = self.secret_store();
        let secrets_ref = secrets.as_deref();
        let params = self.net_parameters();
        let params_ref = params.as_ref();
        let rt = self.pre_dispatch_runtime();
        let rt_ref = rt.as_deref();
        firing::fire_transition::<E, T, S>(
            self.events.as_ref(),
            self.topology.as_ref(),
            &self.executor,
            &self.effect_handlers,
            &self.execution_mode,
            &self.replay_cursor,
            self.workflow_id(),
            &marking,
            transition_id,
            registry_ref,
            secrets_ref,
            params_ref,
            rt_ref,
        )
        .await
    }

    // ========================================================================
    // Evaluation (delegates to evaluation module)
    // ========================================================================

    /// Check if a transition is enabled (can fire).
    pub async fn is_enabled(&self, transition_id: &TransitionId) -> Result<bool, ServiceError> {
        let marking = self.get_marking_cached().await;
        evaluation::is_enabled(
            &self.executor,
            self.topology.as_ref(),
            transition_id,
            &marking,
        )
    }

    /// Get all enabled transitions.
    pub async fn enabled_transitions(&self) -> Result<Vec<TransitionId>, ServiceError> {
        let marking = self.get_marking_cached().await;
        evaluation::enabled_transitions(&self.executor, self.topology.as_ref(), &marking)
    }

    /// Get the status of all transitions with reasons for being disabled.
    pub async fn transition_statuses(
        &self,
    ) -> Result<HashMap<TransitionId, TransitionStatusDetail>, ServiceError> {
        let marking = self.get_marking_cached().await;
        evaluation::transition_statuses(&self.executor, self.topology.as_ref(), &marking)
    }

    /// Compute the enabling time for a transition.
    pub async fn transition_enabling_time(
        &self,
        transition_id: &TransitionId,
    ) -> Result<Option<DateTime<Utc>>, ServiceError> {
        let marking = self.get_marking_cached().await;
        evaluation::transition_enabling_time(
            &self.executor,
            self.topology.as_ref(),
            transition_id,
            &marking,
        )
    }

    /// Select the next transition to fire based on enabling time, specificity, and token priority.
    pub async fn select_next_transition(&self) -> Result<Option<TransitionId>, ServiceError> {
        let marking = self.get_marking_cached().await;
        evaluation::select_next_transition(&self.executor, self.topology.as_ref(), &marking)
    }

    /// Evaluate transitions until quiescence or max steps reached.
    ///
    /// Only one evaluation can run at a time per service instance. If another
    /// call is already in progress, this returns immediately with zero steps.
    /// This prevents the eval loop and HTTP handler from racing on the same
    /// marking and double-firing effect transitions.
    pub async fn evaluate_until_quiescent(
        &self,
        max_steps: usize,
    ) -> Result<EvaluateResult, ServiceError> {
        // Try to acquire the eval lock. If another evaluation is in progress,
        // return immediately — the active evaluation will pick up any new tokens.
        let _guard = match self.eval_lock.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                return Ok(EvaluateResult {
                    steps_executed: 0,
                    transitions_fired: Vec::new(),
                    final_state: crate::evaluation::EvaluateFinalState::Quiescent,
                    events: Vec::new(),
                    terminal_reached: None,
                });
            }
        };

        let registry = self.schema_registry();
        let config = self.execution_config();
        let registry_ref = if config.validate_output_schemas {
            registry.as_deref()
        } else {
            None
        };
        let secrets = self.secret_store();
        let secrets_ref = secrets.as_deref();
        let params = self.net_parameters();
        let params_ref = params.as_ref();
        let rt = self.pre_dispatch_runtime();
        let rt_ref = rt.as_deref();
        evaluation::evaluate_until_quiescent(
            self.events.as_ref(),
            self.topology.as_ref(),
            self.projection.as_ref(),
            &self.executor,
            &self.effect_handlers,
            &self.execution_mode,
            &self.replay_cursor,
            &self.workflow_id,
            &self.cached_state,
            max_steps,
            registry_ref,
            secrets_ref,
            params_ref,
            rt_ref,
        )
        .await
    }

    // ========================================================================
    // State queries
    // ========================================================================

    /// Get all events.
    pub async fn get_events(&self) -> Vec<PersistedEvent> {
        self.events.all_events().await
    }

    /// Append a raw domain event to the event log.
    ///
    /// Use sparingly — most events should be emitted through higher-level
    /// service methods. This is intended for lifecycle events (`NetCreated`,
    /// `NetCompleted`, `NetCancelled`) that don't involve token manipulation.
    pub async fn append_event(
        &self,
        event: DomainEvent,
    ) -> Result<PersistedEvent, crate::EventStoreError> {
        self.events.append(event).await
    }

    /// Get the current marking.
    pub async fn get_marking(&self) -> Marking {
        self.get_marking_cached().await
    }

    /// Get the current marking, using a cache to avoid full event replay.
    async fn get_marking_cached(&self) -> Marking {
        get_marking_cached(
            self.events.as_ref(),
            self.projection.as_ref(),
            &self.cached_state,
        )
        .await
    }

    /// Invalidate the cached marking state.
    fn invalidate_cache(&self) {
        *self.cached_state.write().unwrap() = None;
    }

    /// Reset the engine (clear all events and restore initial state).
    pub async fn reset(&self) -> Result<(), ServiceError> {
        self.events.reset().await;
        self.invalidate_cache();
        *self.replay_cursor.write().unwrap() = 0;

        if let Some(net) = self.topology.get_topology() {
            self.events
                .append(DomainEvent::NetInitialized { net })
                .await?;
        }

        let initial = self.initial_tokens.read().unwrap().clone();
        for (place_id, color) in initial.iter() {
            let token = Token::new(color.clone());
            self.events
                .append(DomainEvent::TokenCreated {
                    token,
                    place_id: place_id.clone(),
                    place_name: None,
                    workflow_id: None,
                    signal_key: None,
                    dedup_id: None,
                })
                .await?;
        }

        Ok(())
    }

    // ========================================================================
    // Signal / place resolution
    // ========================================================================

    /// Resolve a string place ID to a PlaceId.
    pub fn resolve_place_id(&self, place_id_str: &str) -> Result<PlaceId, ServiceError> {
        let pid = PlaceId(place_id_str.to_string());

        let net = self
            .topology
            .get_topology()
            .ok_or(ServiceError::NoTopology)?;

        // Direct ID match
        if net.places.contains_key(&pid) {
            return Ok(pid);
        }

        // Fallback: scan by place name
        for (id, place) in &net.places {
            if place.name == place_id_str {
                return Ok(id.clone());
            }
        }

        Err(ServiceError::PlaceNotFound(pid))
    }

    /// Inject a signal token into a place by string ID.
    pub async fn inject_signal(
        &self,
        place_id_str: &str,
        token_data: JsonValue,
    ) -> Result<PersistedEvent, ServiceError> {
        let place_id = self.resolve_place_id(place_id_str)?;
        let color = json_to_token_color(&token_data);
        self.create_token(place_id, color).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply_event_to_marking;
    use crate::evaluation::EvaluateFinalState;
    use petri_domain::{Arc as PetriArc, PetriNet, Place, Port, TokenColor, Transition};
    use std::sync::Arc;

    // Simple test implementations of the repository traits
    struct TestEventRepo {
        events: RwLock<Vec<PersistedEvent>>,
    }

    impl TestEventRepo {
        fn new() -> Self {
            Self {
                events: RwLock::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl EventRepository for TestEventRepo {
        async fn append(
            &self,
            event: DomainEvent,
        ) -> Result<PersistedEvent, crate::EventStoreError> {
            let mut events = self.events.write().unwrap();
            let sequence = events.len() as u64;
            let previous_hash = events.last().map(|e| e.hash.clone());
            let persisted = PersistedEvent::new(sequence, event, previous_hash);
            events.push(persisted.clone());
            Ok(persisted)
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
        }

        async fn current_sequence(&self) -> u64 {
            self.events.read().unwrap().len() as u64
        }
    }

    struct TestTopologyRepo {
        topology: RwLock<Option<PetriNet>>,
    }

    impl TestTopologyRepo {
        fn new() -> Self {
            Self {
                topology: RwLock::new(None),
            }
        }
    }

    impl TopologyRepository for TestTopologyRepo {
        fn get_topology(&self) -> Option<PetriNet> {
            self.topology.read().unwrap().clone()
        }

        fn set_topology(&self, net: PetriNet) {
            *self.topology.write().unwrap() = Some(net);
        }

        fn clear(&self) {
            *self.topology.write().unwrap() = None;
        }

        fn update_transition_script(
            &self,
            transition_id: &TransitionId,
            script: String,
            guard: Option<String>,
        ) -> bool {
            if let Some(ref mut net) = *self.topology.write().unwrap() {
                if let Some(t) = net.transitions.get_mut(transition_id) {
                    t.script = script;
                    t.guard = guard;
                    return true;
                }
            }
            false
        }
    }

    struct TestStateProjection;

    impl TestStateProjection {
        fn new() -> Self {
            Self
        }
    }

    impl StateProjection for TestStateProjection {
        fn project(&self, events: &[PersistedEvent]) -> Marking {
            let mut marking = Marking::new();
            for persisted in events {
                apply_event_to_marking(&mut marking, &persisted.event);
            }
            marking
        }
    }

    fn create_test_service() -> PetriNetService<TestEventRepo, TestTopologyRepo, TestStateProjection>
    {
        let events = Arc::new(TestEventRepo::new());
        let topology = Arc::new(TestTopologyRepo::new());
        let state = Arc::new(TestStateProjection::new());
        PetriNetService::new(events, topology, state)
    }

    fn create_simple_net() -> PetriNet {
        let mut net = PetriNet::new();

        let input = Place::internal("input");
        let output = Place::internal("output");

        let transition = Transition::new("passthrough", "#{out: inp}")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out")]);

        let input_id = input.id.clone();
        let output_id = output.id.clone();
        let transition_id = transition.id.clone();

        net.add_place(input);
        net.add_place(output);
        net.add_transition(transition);

        net.add_arc(PetriArc::input(input_id, transition_id.clone(), "inp"));
        net.add_arc(PetriArc::output(transition_id, "out", output_id));

        net
    }

    #[tokio::test]
    async fn test_evaluate_until_quiescent_returns_events() {
        let service = create_test_service();
        let net = create_simple_net();

        let input_id = net
            .places
            .values()
            .find(|p| p.name == "input")
            .unwrap()
            .id
            .clone();

        service.initialize(net).await.unwrap();
        service
            .create_token(input_id, TokenColor::Unit)
            .await
            .unwrap();

        let result = service.evaluate_until_quiescent(100).await.unwrap();

        assert_eq!(result.steps_executed, 1);
        assert_eq!(result.final_state, EvaluateFinalState::Quiescent);
        assert_eq!(result.events.len(), 1);

        match &result.events[0].event {
            DomainEvent::TransitionFired {
                transition_id,
                produced_tokens,
                ..
            } => {
                assert!(!produced_tokens.is_empty(), "Should have produced tokens");
                assert_eq!(result.transitions_fired.len(), 1);
                assert_eq!(&result.transitions_fired[0].0, transition_id);
            }
            _ => panic!("Expected TransitionFired event"),
        }
    }

    #[tokio::test]
    async fn test_evaluate_until_quiescent_multiple_steps() {
        let service = create_test_service();

        let mut net = PetriNet::new();

        let place_a = Place::internal("A");
        let place_b = Place::internal("B");
        let place_c = Place::internal("C");

        let t1 = Transition::new("T1", "#{out: inp}")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out")]);
        let t2 = Transition::new("T2", "#{out: inp}")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out")]);

        let a_id = place_a.id.clone();
        let b_id = place_b.id.clone();
        let c_id = place_c.id.clone();
        let t1_id = t1.id.clone();
        let t2_id = t2.id.clone();

        net.add_place(place_a);
        net.add_place(place_b);
        net.add_place(place_c);
        net.add_transition(t1);
        net.add_transition(t2);

        net.add_arc(PetriArc::input(a_id.clone(), t1_id.clone(), "inp"));
        net.add_arc(PetriArc::output(t1_id, "out", b_id.clone()));
        net.add_arc(PetriArc::input(b_id, t2_id.clone(), "inp"));
        net.add_arc(PetriArc::output(t2_id, "out", c_id));

        service.initialize(net).await.unwrap();
        service.create_token(a_id, TokenColor::Unit).await.unwrap();

        let result = service.evaluate_until_quiescent(100).await.unwrap();

        assert_eq!(result.steps_executed, 2);
        assert_eq!(result.final_state, EvaluateFinalState::Quiescent);
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.transitions_fired.len(), 2);
    }

    #[tokio::test]
    async fn test_evaluate_until_quiescent_empty_events_when_no_transitions() {
        let service = create_test_service();
        let net = create_simple_net();

        service.initialize(net).await.unwrap();

        let result = service.evaluate_until_quiescent(100).await.unwrap();

        assert_eq!(result.steps_executed, 0);
        assert_eq!(result.final_state, EvaluateFinalState::Quiescent);
        assert!(result.events.is_empty());
        assert!(result.transitions_fired.is_empty());
    }

    #[tokio::test]
    async fn test_evaluate_until_quiescent_limit_reached() {
        let service = create_test_service();

        let mut net = PetriNet::new();

        let place = Place::internal("loop");
        let transition = Transition::new("loop_t", "#{out: inp}")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out")]);

        let place_id = place.id.clone();
        let t_id = transition.id.clone();

        net.add_place(place);
        net.add_transition(transition);

        net.add_arc(PetriArc::input(place_id.clone(), t_id.clone(), "inp"));
        net.add_arc(PetriArc::output(t_id, "out", place_id.clone()));

        service.initialize(net).await.unwrap();
        service
            .create_token(place_id, TokenColor::Unit)
            .await
            .unwrap();

        let result = service.evaluate_until_quiescent(5).await.unwrap();

        assert_eq!(result.steps_executed, 5);
        assert_eq!(result.final_state, EvaluateFinalState::LimitReached);
        assert_eq!(result.events.len(), 5);
    }

    // ========================================================================
    // Effect transition tests
    // ========================================================================

    use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockEffectHandler {
        name: String,
        execute_count: AtomicUsize,
        replay_count: AtomicUsize,
        output_tokens: RwLock<HashMap<String, serde_json::Value>>,
        output_result: RwLock<serde_json::Value>,
        last_execute_input: RwLock<Option<EffectInput>>,
        last_replay_input: RwLock<Option<EffectInput>>,
        last_replay_result: RwLock<Option<serde_json::Value>>,
    }

    impl MockEffectHandler {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                execute_count: AtomicUsize::new(0),
                replay_count: AtomicUsize::new(0),
                output_tokens: RwLock::new(HashMap::new()),
                output_result: RwLock::new(serde_json::json!({"mock": true})),
                last_execute_input: RwLock::new(None),
                last_replay_input: RwLock::new(None),
                last_replay_result: RwLock::new(None),
            }
        }

        fn with_output(
            self,
            port: &str,
            data: serde_json::Value,
            result: serde_json::Value,
        ) -> Self {
            self.output_tokens
                .write()
                .unwrap()
                .insert(port.to_string(), data);
            *self.output_result.write().unwrap() = result;
            self
        }
    }

    #[async_trait::async_trait]
    impl EffectHandler for MockEffectHandler {
        async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
            *self.last_execute_input.write().unwrap() = Some(input);
            self.execute_count.fetch_add(1, Ordering::SeqCst);
            Ok(EffectOutput {
                tokens: self.output_tokens.read().unwrap().clone(),
                result: self.output_result.read().unwrap().clone(),
            })
        }

        fn replay(&self, input: &EffectInput, stored_result: &serde_json::Value) {
            self.replay_count.fetch_add(1, Ordering::SeqCst);
            *self.last_replay_input.write().unwrap() = Some(EffectInput {
                transition_id: input.transition_id.clone(),
                inputs: input.inputs.clone(),
                config: input.config.clone(),
                read_inputs: input.read_inputs.clone(),
                process_step: input.process_step.clone(),
            });
            *self.last_replay_result.write().unwrap() = Some(stored_result.clone());
        }

        fn name(&self) -> &str {
            &self.name
        }
    }

    fn create_effect_net(
        handler_id: &str,
        effect_config: Option<serde_json::Value>,
    ) -> (PetriNet, PlaceId, PlaceId, TransitionId) {
        let mut net = PetriNet::new();

        let input = Place::internal("input");
        let output = Place::internal("output");

        let mut transition = Transition::new("effect_transition", "")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out")])
            .with_effect_handler(handler_id);

        if let Some(config) = effect_config {
            transition = transition.with_effect_config(config);
        }

        let input_id = input.id.clone();
        let output_id = output.id.clone();
        let transition_id = transition.id.clone();

        net.add_place(input);
        net.add_place(output);
        net.add_transition(transition);

        net.add_arc(PetriArc::input(
            input_id.clone(),
            transition_id.clone(),
            "inp",
        ));
        net.add_arc(PetriArc::output(
            transition_id.clone(),
            "out",
            output_id.clone(),
        ));

        (net, input_id, output_id, transition_id)
    }

    #[tokio::test]
    async fn test_fire_effect_transition_live_mode() {
        let service = create_test_service();
        let effect_config = serde_json::json!({"static_key": "static_value"});
        let (net, input_id, output_id, _transition_id) =
            create_effect_net("test_handler", Some(effect_config));

        let handler = Arc::new(MockEffectHandler::new("test_handler").with_output(
            "out",
            serde_json::json!({"result": 42}),
            serde_json::json!({"status": "ok"}),
        ));
        let handler_ref = handler.clone();

        service
            .register_effect_handler("test_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();
        service
            .create_token(
                input_id.clone(),
                TokenColor::Data(serde_json::json!({"query": "test"})),
            )
            .await
            .unwrap();

        let result = service.evaluate_until_quiescent(10).await.unwrap();

        assert_eq!(result.steps_executed, 1);
        assert_eq!(
            handler_ref.execute_count.load(Ordering::SeqCst),
            1,
            "Handler should be called once"
        );

        // Note: MockEffectHandler doesn't currently store the config it received,
        // but the build/run succeeded, meaning EffectInput structure was satisfied.

        assert_eq!(
            handler_ref.replay_count.load(Ordering::SeqCst),
            0,
            "Replay should not be called"
        );

        match &result.events[0].event {
            DomainEvent::EffectCompleted {
                effect_handler_id,
                effect_result,
                consumed_tokens,
                produced_tokens,
                ..
            } => {
                assert_eq!(effect_handler_id, "test_handler");
                assert_eq!(*effect_result, serde_json::json!({"status": "ok"}));
                assert_eq!(consumed_tokens.len(), 1);
                assert_eq!(produced_tokens.len(), 1);
                assert_eq!(produced_tokens[0].0, output_id);
            }
            other => panic!("Expected EffectCompleted, got {:?}", other),
        }

        let events = service.get_events().await;
        let marking = TestStateProjection::new().project(&events);
        assert_eq!(marking.token_count(&input_id), 0);
        assert_eq!(marking.token_count(&output_id), 1);
    }

    #[tokio::test]
    async fn test_fire_effect_transition_replay_mode() {
        let service = create_test_service();
        let (net, input_id, output_id, transition_id) = create_effect_net("test_handler", None);

        let handler = Arc::new(MockEffectHandler::new("test_handler").with_output(
            "out",
            serde_json::json!({"result": 99}),
            serde_json::json!({"replayed": true}),
        ));
        let handler_ref = handler.clone();

        service
            .register_effect_handler("test_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();

        let prior_token_id = TokenId::new();
        let stored_produced_token =
            Token::new(TokenColor::Data(serde_json::json!({"stored_result": true})));
        service
            .events
            .append(DomainEvent::EffectCompleted {
                transition_id: transition_id.clone(),
                transition_name: Some("effect_transition".to_string()),
                consumed_tokens: vec![(input_id.clone(), prior_token_id)],
                produced_tokens: vec![(output_id.clone(), stored_produced_token.clone())],
                effect_handler_id: "test_handler".to_string(),
                effect_result: serde_json::json!({"original_result": "from_live_run"}),
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            })
            .await
            .unwrap();

        service
            .create_token(
                input_id.clone(),
                TokenColor::Data(serde_json::json!({"query": "replay_test"})),
            )
            .await
            .unwrap();

        service.set_execution_mode(ExecutionMode::Replay);

        let result = service.evaluate_until_quiescent(1).await.unwrap();

        assert_eq!(result.steps_executed, 1);
        assert_eq!(
            handler_ref.execute_count.load(Ordering::SeqCst),
            0,
            "Execute should NOT be called in replay mode"
        );
        assert_eq!(
            handler_ref.replay_count.load(Ordering::SeqCst),
            1,
            "Replay should be called once"
        );

        let last_result = handler_ref.last_replay_result.read().unwrap();
        assert_eq!(
            *last_result,
            Some(serde_json::json!({"original_result": "from_live_run"}))
        );

        let last_input = handler_ref.last_replay_input.read().unwrap();
        assert!(last_input.is_some(), "Replay should have received input");
        let input = last_input.as_ref().unwrap();
        assert_eq!(input.transition_id, transition_id);

        match &result.events[0].event {
            DomainEvent::EffectCompleted {
                effect_handler_id,
                effect_result,
                produced_tokens,
                ..
            } => {
                assert_eq!(effect_handler_id, "test_handler");
                assert_eq!(
                    *effect_result,
                    serde_json::json!({"original_result": "from_live_run"})
                );
                assert_eq!(produced_tokens.len(), 1);
                assert_eq!(produced_tokens[0].1.id, stored_produced_token.id);
            }
            other => panic!("Expected EffectCompleted, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_effect_transition_no_registry_returns_error() {
        let service = create_test_service();
        let (net, input_id, _output_id, _transition_id) =
            create_effect_net("missing_handler", None);

        service.initialize(net).await.unwrap();
        service
            .create_token(input_id, TokenColor::Unit)
            .await
            .unwrap();

        let result = service.evaluate_until_quiescent(10).await;
        match result {
            Ok(r) => {
                assert_eq!(r.steps_executed, 0);
            }
            Err(_) => {
                // Also acceptable
            }
        }
    }

    #[tokio::test]
    async fn test_effect_transition_produces_correct_marking() {
        let service = create_test_service();
        let (net, input_id, output_id, _transition_id) = create_effect_net("mark_handler", None);

        let handler = Arc::new(MockEffectHandler::new("mark_handler").with_output(
            "out",
            serde_json::json!({"value": 100}),
            serde_json::json!({"done": true}),
        ));

        service
            .register_effect_handler("mark_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();
        service
            .create_token(
                input_id.clone(),
                TokenColor::Data(serde_json::json!({"x": 1})),
            )
            .await
            .unwrap();

        let _result = service.evaluate_until_quiescent(10).await.unwrap();

        let events = service.get_events().await;
        let marking = TestStateProjection::new().project(&events);

        assert_eq!(marking.token_count(&input_id), 0);
        assert_eq!(marking.token_count(&output_id), 1);

        let output_tokens = marking.tokens_at(&output_id);
        assert_eq!(output_tokens.len(), 1);
        match &output_tokens[0].color {
            TokenColor::Data(data) => {
                assert_eq!(*data, serde_json::json!({"value": 100}));
            }
            other => panic!("Expected Data color, got {:?}", other),
        }
    }

    // ========================================================================
    // Replay cursor tests
    // ========================================================================

    /// Build an effect net that loops: output feeds back to input.
    fn create_effect_loop_net(handler_id: &str) -> (PetriNet, PlaceId, TransitionId) {
        let mut net = PetriNet::new();

        let place = Place::internal("loop_place");

        let transition = Transition::new("loop_effect", "")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out")])
            .with_effect_handler(handler_id);

        let place_id = place.id.clone();
        let transition_id = transition.id.clone();

        net.add_place(place);
        net.add_transition(transition);

        net.add_arc(PetriArc::input(
            place_id.clone(),
            transition_id.clone(),
            "inp",
        ));
        net.add_arc(PetriArc::output(
            transition_id.clone(),
            "out",
            place_id.clone(),
        ));

        (net, place_id, transition_id)
    }

    /// Handler that returns a different result each invocation.
    struct CountingEffectHandler {
        call_count: AtomicUsize,
    }

    impl CountingEffectHandler {
        fn new() -> Self {
            Self {
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl EffectHandler for CountingEffectHandler {
        async fn execute(&self, _input: EffectInput) -> Result<EffectOutput, EffectError> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(EffectOutput {
                tokens: {
                    let mut m = HashMap::new();
                    m.insert("out".to_string(), serde_json::json!({"generation": n}));
                    m
                },
                result: serde_json::json!({"generation": n}),
            })
        }

        fn replay(&self, _input: &EffectInput, _stored_result: &serde_json::Value) {}

        fn name(&self) -> &str {
            "counting_handler"
        }
    }

    #[tokio::test]
    async fn test_replay_cursor_second_iteration() {
        let service = create_test_service();
        let (net, place_id, _transition_id) = create_effect_loop_net("counting");

        let handler = Arc::new(CountingEffectHandler::new());
        service
            .register_effect_handler("counting", handler)
            .unwrap();

        service.initialize(net).await.unwrap();
        service
            .create_token(place_id.clone(), TokenColor::Unit)
            .await
            .unwrap();

        // Fire 2 iterations in live mode
        let result = service.evaluate_until_quiescent(2).await.unwrap();
        assert_eq!(result.steps_executed, 2);

        // Verify we got generation 0 and generation 1
        match &result.events[0].event {
            DomainEvent::EffectCompleted { effect_result, .. } => {
                assert_eq!(effect_result["generation"], 0);
            }
            other => panic!("Expected EffectCompleted, got {:?}", other),
        }
        match &result.events[1].event {
            DomainEvent::EffectCompleted { effect_result, .. } => {
                assert_eq!(effect_result["generation"], 1);
            }
            other => panic!("Expected EffectCompleted, got {:?}", other),
        }

        // Now switch to replay mode and replay both
        service.set_execution_mode(ExecutionMode::Replay);
        // Re-seed a token so transitions can fire
        service
            .create_token(place_id.clone(), TokenColor::Unit)
            .await
            .unwrap();

        let replay_result = service.evaluate_until_quiescent(2).await.unwrap();
        assert_eq!(replay_result.steps_executed, 2);

        // The second replay must get generation 1, not generation 0
        match &replay_result.events[0].event {
            DomainEvent::EffectCompleted { effect_result, .. } => {
                assert_eq!(
                    effect_result["generation"], 0,
                    "First replay should get gen 0"
                );
            }
            other => panic!("Expected EffectCompleted, got {:?}", other),
        }
        match &replay_result.events[1].event {
            DomainEvent::EffectCompleted { effect_result, .. } => {
                assert_eq!(
                    effect_result["generation"], 1,
                    "Second replay should get gen 1"
                );
            }
            other => panic!("Expected EffectCompleted, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_replay_cursor_resets_on_reset() {
        let service = create_test_service();
        let (net, place_id, _transition_id) = create_effect_loop_net("counting");

        let handler = Arc::new(CountingEffectHandler::new());
        service
            .register_effect_handler("counting", handler)
            .unwrap();

        service.initialize(net).await.unwrap();
        service
            .create_token(place_id.clone(), TokenColor::Unit)
            .await
            .unwrap();

        // Fire once in live mode to advance cursor state
        service.evaluate_until_quiescent(1).await.unwrap();

        // Reset should clear the cursor
        service.reset().await.unwrap();
        assert_eq!(*service.replay_cursor.read().unwrap(), 0);
    }

    #[tokio::test]
    async fn test_replay_cursor_resets_on_mode_switch() {
        let service = create_test_service();
        let net = create_simple_net();
        service.initialize(net).await.unwrap();

        // Set to Replay should reset cursor
        service.set_execution_mode(ExecutionMode::Replay);
        assert_eq!(*service.replay_cursor.read().unwrap(), 0);

        // Even if we manually bump it (not normal use), switching to Replay again resets
        *service.replay_cursor.write().unwrap() = 42;
        service.set_execution_mode(ExecutionMode::Replay);
        assert_eq!(*service.replay_cursor.read().unwrap(), 0);
    }

    // ========================================================================
    // EffectFailed / error routing tests
    // ========================================================================

    /// Handler that always fails.
    struct FailingEffectHandler {
        error_msg: String,
    }

    impl FailingEffectHandler {
        fn new(msg: &str) -> Self {
            Self {
                error_msg: msg.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl EffectHandler for FailingEffectHandler {
        async fn execute(&self, _input: EffectInput) -> Result<EffectOutput, EffectError> {
            Err(EffectError::ExecutionFailed(self.error_msg.clone()))
        }

        fn name(&self) -> &str {
            "failing_handler"
        }
    }

    /// Create an effect net with an `_error` output port.
    fn create_effect_net_with_error_port(
        handler_id: &str,
    ) -> (PetriNet, PlaceId, PlaceId, PlaceId, TransitionId) {
        let mut net = PetriNet::new();

        let input = Place::internal("input");
        let output = Place::internal("output");
        let error_place = Place::internal("error_place");

        let transition = Transition::new("effect_with_error", "")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out"), Port::new("_error")])
            .with_effect_handler(handler_id);

        let input_id = input.id.clone();
        let output_id = output.id.clone();
        let error_id = error_place.id.clone();
        let transition_id = transition.id.clone();

        net.add_place(input);
        net.add_place(output);
        net.add_place(error_place);
        net.add_transition(transition);

        net.add_arc(PetriArc::input(
            input_id.clone(),
            transition_id.clone(),
            "inp",
        ));
        net.add_arc(PetriArc::output(
            transition_id.clone(),
            "out",
            output_id.clone(),
        ));
        net.add_arc(PetriArc::output(
            transition_id.clone(),
            "_error",
            error_id.clone(),
        ));

        (net, input_id, output_id, error_id, transition_id)
    }

    #[tokio::test]
    async fn test_effect_failure_with_error_port() {
        let service = create_test_service();
        let (net, input_id, output_id, error_id, _transition_id) =
            create_effect_net_with_error_port("fail_handler");

        let handler = Arc::new(FailingEffectHandler::new("something went wrong"));
        service
            .register_effect_handler("fail_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();
        service
            .create_token(
                input_id.clone(),
                TokenColor::Data(serde_json::json!({"x": 1})),
            )
            .await
            .unwrap();

        let result = service.evaluate_until_quiescent(10).await.unwrap();
        assert_eq!(result.steps_executed, 1, "Should fire once (error-routed)");

        // Should emit EffectFailed with tokens_consumed=true
        match &result.events[0].event {
            DomainEvent::EffectFailed {
                effect_handler_id,
                error_message,
                tokens_consumed,
                consumed_tokens,
                produced_tokens,
                ..
            } => {
                assert_eq!(effect_handler_id, "fail_handler");
                assert!(error_message.contains("something went wrong"));
                assert!(*tokens_consumed, "tokens_consumed should be true");
                assert_eq!(consumed_tokens.len(), 1, "Should consume 1 token");
                assert_eq!(produced_tokens.len(), 1, "Should produce error token");
                assert_eq!(produced_tokens[0].0, error_id);
            }
            other => panic!("Expected EffectFailed, got {:?}", other),
        }

        // Marking: input empty, output empty, error place has 1 token
        let events = service.get_events().await;
        let marking = TestStateProjection::new().project(&events);
        assert_eq!(marking.token_count(&input_id), 0);
        assert_eq!(marking.token_count(&output_id), 0);
        assert_eq!(marking.token_count(&error_id), 1);

        // Error token should contain structured error data
        let error_tokens = marking.tokens_at(&error_id);
        match &error_tokens[0].color {
            TokenColor::Data(data) => {
                assert!(data["error"]
                    .as_str()
                    .unwrap()
                    .contains("something went wrong"));
                assert_eq!(data["handler_id"], "fail_handler");
            }
            other => panic!("Expected Data color, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_effect_failure_without_error_port() {
        let service = create_test_service();
        // Use the standard effect net (no _error port)
        let (net, input_id, output_id, _transition_id) = create_effect_net("fail_handler", None);

        let handler = Arc::new(FailingEffectHandler::new("no error port"));
        service
            .register_effect_handler("fail_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();
        service
            .create_token(
                input_id.clone(),
                TokenColor::Data(serde_json::json!({"x": 1})),
            )
            .await
            .unwrap();

        let result = service.evaluate_until_quiescent(10).await;

        // Should return Err(ServiceError::EffectFailed)
        match result {
            Err(ServiceError::EffectFailed {
                handler_id,
                message,
                ..
            }) => {
                assert_eq!(handler_id, "fail_handler");
                assert!(message.contains("no error port"));
            }
            other => panic!("Expected Err(EffectFailed), got {:?}", other),
        }

        // Tokens should still be in the input place (not consumed)
        let events = service.get_events().await;
        let marking = TestStateProjection::new().project(&events);
        assert_eq!(
            marking.token_count(&input_id),
            1,
            "Token should stay in input place"
        );
        assert_eq!(marking.token_count(&output_id), 0);

        // EffectFailed event should be in the log
        let effect_failed = events
            .iter()
            .find(|e| matches!(&e.event, DomainEvent::EffectFailed { .. }));
        assert!(
            effect_failed.is_some(),
            "EffectFailed event should be in log"
        );
        match &effect_failed.unwrap().event {
            DomainEvent::EffectFailed {
                tokens_consumed, ..
            } => {
                assert!(!tokens_consumed, "tokens_consumed should be false");
            }
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn test_effect_failure_eval_loop_stops_without_error_port() {
        let service = create_test_service();

        // Build a chain: A -> T1(effect, fails) -> B -> T2(script) -> C
        let mut net = PetriNet::new();
        let place_a = Place::internal("A");
        let place_b = Place::internal("B");
        let place_c = Place::internal("C");

        let t1 = Transition::new("T1_effect", "")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out")])
            .with_effect_handler("fail_handler");

        let t2 = Transition::new("T2_script", "#{out: inp}")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out")]);

        let a_id = place_a.id.clone();
        let b_id = place_b.id.clone();
        let c_id = place_c.id.clone();
        let t1_id = t1.id.clone();
        let t2_id = t2.id.clone();

        net.add_place(place_a);
        net.add_place(place_b);
        net.add_place(place_c);
        net.add_transition(t1);
        net.add_transition(t2);

        net.add_arc(PetriArc::input(a_id.clone(), t1_id.clone(), "inp"));
        net.add_arc(PetriArc::output(t1_id, "out", b_id.clone()));
        net.add_arc(PetriArc::input(b_id, t2_id.clone(), "inp"));
        net.add_arc(PetriArc::output(t2_id, "out", c_id.clone()));

        let handler = Arc::new(FailingEffectHandler::new("stop the loop"));
        service
            .register_effect_handler("fail_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();
        service.create_token(a_id, TokenColor::Unit).await.unwrap();

        let result = service.evaluate_until_quiescent(10).await;
        match result {
            Err(ServiceError::EffectFailed { .. }) => {
                // Expected — eval loop stops at T1
            }
            other => panic!("Expected EffectFailed error, got {:?}", other),
        }

        // C should have no tokens (T2 never fired)
        let events = service.get_events().await;
        let marking = TestStateProjection::new().project(&events);
        assert_eq!(marking.token_count(&c_id), 0);
    }

    #[tokio::test]
    async fn test_effect_failure_eval_loop_continues_with_error_port() {
        let service = create_test_service();

        // Build: input -> T1(effect+_error) -> {output, error_place}
        // Then: error_place -> T2(script) -> final_place
        let mut net = PetriNet::new();
        let input = Place::internal("input");
        let output = Place::internal("output");
        let error_place = Place::internal("error_place");
        let final_place = Place::internal("final");

        let t1 = Transition::new("T1_effect", "")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out"), Port::new("_error")])
            .with_effect_handler("fail_handler");

        let t2 = Transition::new("T2_handle_error", "#{out: inp}")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out")]);

        let input_id = input.id.clone();
        let output_id = output.id.clone();
        let error_id = error_place.id.clone();
        let final_id = final_place.id.clone();
        let t1_id = t1.id.clone();
        let t2_id = t2.id.clone();

        net.add_place(input);
        net.add_place(output);
        net.add_place(error_place);
        net.add_place(final_place);
        net.add_transition(t1);
        net.add_transition(t2);

        net.add_arc(PetriArc::input(input_id.clone(), t1_id.clone(), "inp"));
        net.add_arc(PetriArc::output(t1_id.clone(), "out", output_id.clone()));
        net.add_arc(PetriArc::output(t1_id, "_error", error_id.clone()));
        net.add_arc(PetriArc::input(error_id.clone(), t2_id.clone(), "inp"));
        net.add_arc(PetriArc::output(t2_id, "out", final_id.clone()));

        let handler = Arc::new(FailingEffectHandler::new("handled error"));
        service
            .register_effect_handler("fail_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();
        service
            .create_token(
                input_id.clone(),
                TokenColor::Data(serde_json::json!({"x": 1})),
            )
            .await
            .unwrap();

        // Should succeed — error is routed through T1 -> error_place -> T2 -> final
        let result = service.evaluate_until_quiescent(10).await.unwrap();
        assert_eq!(result.steps_executed, 2, "T1 (error-routed) + T2 (script)");

        let events = service.get_events().await;
        let marking = TestStateProjection::new().project(&events);
        assert_eq!(marking.token_count(&input_id), 0);
        assert_eq!(marking.token_count(&error_id), 0);
        assert_eq!(
            marking.token_count(&final_id),
            1,
            "Error token should flow to final place"
        );
    }

    #[tokio::test]
    async fn test_replay_effect_failed_with_error_port() {
        let service = create_test_service();
        let (net, input_id, _output_id, error_id, transition_id) =
            create_effect_net_with_error_port("fail_handler");

        let handler = Arc::new(FailingEffectHandler::new("replayed error"));
        service
            .register_effect_handler("fail_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();

        // Pre-populate log with an EffectFailed event (tokens_consumed: true)
        let prior_token_id = TokenId::new();
        let stored_error_token =
            Token::new(TokenColor::Data(serde_json::json!({"error": "stored"})));
        service
            .events
            .append(DomainEvent::EffectFailed {
                transition_id: transition_id.clone(),
                transition_name: Some("effect_with_error".to_string()),
                consumed_tokens: vec![(input_id.clone(), prior_token_id)],
                produced_tokens: vec![(error_id.clone(), stored_error_token.clone())],
                effect_handler_id: "fail_handler".to_string(),
                error_message: "stored failure".to_string(),
                tokens_consumed: true,
                input_data: None,
                retryable: true,
            })
            .await
            .unwrap();

        // Add a fresh token so the transition is enabled
        service
            .create_token(input_id.clone(), TokenColor::Unit)
            .await
            .unwrap();
        service.set_execution_mode(ExecutionMode::Replay);

        let result = service.evaluate_until_quiescent(1).await.unwrap();
        assert_eq!(result.steps_executed, 1);

        // Should replay as EffectFailed with tokens_consumed=true → Ok
        match &result.events[0].event {
            DomainEvent::EffectFailed {
                tokens_consumed,
                error_message,
                ..
            } => {
                assert!(*tokens_consumed);
                assert_eq!(error_message, "stored failure");
            }
            other => panic!("Expected EffectFailed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_replay_effect_failed_without_error_port() {
        let service = create_test_service();
        let (net, input_id, _output_id, transition_id) = create_effect_net("fail_handler", None);

        let handler = Arc::new(FailingEffectHandler::new("replayed error"));
        service
            .register_effect_handler("fail_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();

        // Pre-populate log with an EffectFailed event (tokens_consumed: false)
        let prior_token_id = TokenId::new();
        service
            .events
            .append(DomainEvent::EffectFailed {
                transition_id: transition_id.clone(),
                transition_name: Some("effect_transition".to_string()),
                consumed_tokens: vec![(input_id.clone(), prior_token_id)],
                produced_tokens: vec![],
                effect_handler_id: "fail_handler".to_string(),
                error_message: "stored failure no port".to_string(),
                tokens_consumed: false,
                input_data: None,
                retryable: true,
            })
            .await
            .unwrap();

        // Add a fresh token so the transition is enabled
        service
            .create_token(input_id.clone(), TokenColor::Unit)
            .await
            .unwrap();
        service.set_execution_mode(ExecutionMode::Replay);

        let result = service.evaluate_until_quiescent(1).await;

        // Should replay as Err(ServiceError::EffectFailed)
        match result {
            Err(ServiceError::EffectFailed {
                handler_id,
                message,
                ..
            }) => {
                assert_eq!(handler_id, "fail_handler");
                assert_eq!(message, "stored failure no port");
            }
            other => panic!("Expected Err(EffectFailed), got {:?}", other),
        }
    }

    // ========================================================================
    // Effect error enrichment tests (inputs + retryable)
    // ========================================================================

    #[tokio::test]
    async fn test_effect_error_token_contains_original_inputs() {
        let service = create_test_service();
        let (net, input_id, _output_id, error_id, _transition_id) =
            create_effect_net_with_error_port("fail_handler");

        let handler = Arc::new(FailingEffectHandler::new("api timeout"));
        service
            .register_effect_handler("fail_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();
        let input_data = serde_json::json!({"url": "http://example.com", "method": "GET"});
        service
            .create_token(input_id.clone(), TokenColor::Data(input_data.clone()))
            .await
            .unwrap();

        let result = service.evaluate_until_quiescent(10).await.unwrap();
        assert_eq!(result.steps_executed, 1);

        // Error token in the _error place should contain original inputs
        let events = service.get_events().await;
        let marking = TestStateProjection::new().project(&events);
        let error_tokens = marking.tokens_at(&error_id);
        assert_eq!(error_tokens.len(), 1);

        match &error_tokens[0].color {
            TokenColor::Data(data) => {
                // Should have the original inputs under "inputs" key
                assert!(
                    data.get("inputs").is_some(),
                    "Error token should contain 'inputs' field"
                );
                let inputs = &data["inputs"];
                assert!(
                    inputs.get("inp").is_some(),
                    "Inputs should have the 'inp' port data"
                );
                assert_eq!(
                    inputs["inp"], input_data,
                    "Input data should match what was provided"
                );

                // Should have retryable flag
                assert_eq!(
                    data["retryable"], true,
                    "ExecutionFailed should be retryable"
                );
            }
            other => panic!("Expected Data color, got {:?}", other),
        }

        // Event should also carry input_data
        match &result.events[0].event {
            DomainEvent::EffectFailed {
                input_data,
                retryable,
                ..
            } => {
                assert!(input_data.is_some(), "Event should carry input_data");
                let data = input_data.as_ref().unwrap();
                assert_eq!(data["inp"], input_data.as_ref().unwrap()["inp"]);
                assert!(*retryable, "ExecutionFailed should be retryable in event");
            }
            other => panic!("Expected EffectFailed, got {:?}", other),
        }
    }

    /// Handler that always returns a fatal (non-retryable) error.
    struct FatalEffectHandler {
        error_msg: String,
    }

    impl FatalEffectHandler {
        fn new(msg: &str) -> Self {
            Self {
                error_msg: msg.to_string(),
            }
        }
    }

    #[async_trait::async_trait]
    impl EffectHandler for FatalEffectHandler {
        async fn execute(&self, _input: EffectInput) -> Result<EffectOutput, EffectError> {
            Err(EffectError::Fatal(self.error_msg.clone()))
        }

        fn name(&self) -> &str {
            "fatal_handler"
        }
    }

    #[tokio::test]
    async fn test_effect_fatal_error_not_retryable() {
        let service = create_test_service();
        let (net, input_id, _output_id, error_id, _transition_id) =
            create_effect_net_with_error_port("fatal_handler");

        let handler = Arc::new(FatalEffectHandler::new("permanent failure"));
        service
            .register_effect_handler("fatal_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();
        service
            .create_token(
                input_id.clone(),
                TokenColor::Data(serde_json::json!({"x": 1})),
            )
            .await
            .unwrap();

        let result = service.evaluate_until_quiescent(10).await.unwrap();
        assert_eq!(result.steps_executed, 1);

        // Error token should have retryable=false
        let events = service.get_events().await;
        let marking = TestStateProjection::new().project(&events);
        let error_tokens = marking.tokens_at(&error_id);

        match &error_tokens[0].color {
            TokenColor::Data(data) => {
                assert_eq!(
                    data["retryable"], false,
                    "Fatal error should not be retryable"
                );
                assert!(data["error"]
                    .as_str()
                    .unwrap()
                    .contains("permanent failure"));
            }
            other => panic!("Expected Data color, got {:?}", other),
        }

        // Event should record retryable=false
        match &result.events[0].event {
            DomainEvent::EffectFailed { retryable, .. } => {
                assert!(!retryable, "Fatal error event should not be retryable");
            }
            other => panic!("Expected EffectFailed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_effect_retry_roundtrip() {
        // Full pattern: effect fails → error token in error place → downstream
        // Rhai transition extracts err.inputs.inp and routes back to input place
        let service = create_test_service();

        let mut net = PetriNet::new();
        let input = Place::internal("requests");
        let output = Place::internal("responses");
        let error_place = Place::internal("errors");

        // Effect transition with _error port
        let effect_t = Transition::new("call_api", "")
            .with_input_ports(vec![Port::new("inp")])
            .with_output_ports(vec![Port::new("out"), Port::new("_error")])
            .with_effect_handler("fail_handler");

        // Retry transition: reads error token, extracts original input, routes back
        let retry_t = Transition::new("retry", "#{inp: err.inputs.inp}")
            .with_input_ports(vec![Port::new("err")])
            .with_output_ports(vec![Port::new("inp")]);

        let input_id = input.id.clone();
        let output_id = output.id.clone();
        let error_id = error_place.id.clone();
        let effect_tid = effect_t.id.clone();
        let retry_tid = retry_t.id.clone();

        net.add_place(input);
        net.add_place(output);
        net.add_place(error_place);
        net.add_transition(effect_t);
        net.add_transition(retry_t);

        // Effect: requests → call_api → {responses, errors}
        net.add_arc(PetriArc::input(input_id.clone(), effect_tid.clone(), "inp"));
        net.add_arc(PetriArc::output(
            effect_tid.clone(),
            "out",
            output_id.clone(),
        ));
        net.add_arc(PetriArc::output(effect_tid, "_error", error_id.clone()));

        // Retry: errors → retry → requests
        net.add_arc(PetriArc::input(error_id.clone(), retry_tid.clone(), "err"));
        net.add_arc(PetriArc::output(retry_tid, "inp", input_id.clone()));

        let handler = Arc::new(FailingEffectHandler::new("transient"));
        service
            .register_effect_handler("fail_handler", handler)
            .unwrap();

        service.initialize(net).await.unwrap();
        let original_data = serde_json::json!({"request_id": "abc123", "payload": "hello"});
        service
            .create_token(input_id.clone(), TokenColor::Data(original_data.clone()))
            .await
            .unwrap();

        // Run 2 steps: effect fails → retry extracts and resubmits
        let result = service.evaluate_until_quiescent(2).await.unwrap();
        assert_eq!(
            result.steps_executed, 2,
            "Effect (fail→error) + retry (extract→resubmit)"
        );

        // After retry, the original input should be back in the requests place
        let events = service.get_events().await;
        let marking = TestStateProjection::new().project(&events);

        assert_eq!(
            marking.token_count(&error_id),
            0,
            "Error place should be empty after retry"
        );
        assert_eq!(marking.token_count(&output_id), 0, "No success output");
        assert_eq!(
            marking.token_count(&input_id),
            1,
            "Original request should be back in input"
        );

        // Verify the resubmitted token contains the original data
        let input_tokens = marking.tokens_at(&input_id);
        match &input_tokens[0].color {
            TokenColor::Data(data) => {
                assert_eq!(
                    *data, original_data,
                    "Retry should preserve original input data"
                );
            }
            other => panic!("Expected Data color, got {:?}", other),
        }
    }

    // ========================================================================
    // Secret resolution tests
    // ========================================================================

    struct TestSecretStore(HashMap<String, String>);

    #[async_trait::async_trait]
    impl aithericon_secrets::SecretStore for TestSecretStore {
        async fn get(&self, key: &str) -> Result<String, aithericon_secrets::SecretError> {
            self.0
                .get(key)
                .cloned()
                .ok_or_else(|| aithericon_secrets::SecretError::NotFound(key.to_string()))
        }
        fn name(&self) -> &str {
            "test"
        }
    }

    #[tokio::test]
    async fn test_secret_resolution_in_effect_config() {
        let service = create_test_service();
        service.set_secret_store(Arc::new(TestSecretStore(HashMap::from([(
            "API_KEY".into(),
            "resolved_api_key".into(),
        )]))));

        let effect_config =
            serde_json::json!({"auth": "{{secret:API_KEY}}", "url": "https://example.com"});
        let (net, input_id, _output_id, _transition_id) =
            create_effect_net("secret_handler", Some(effect_config));

        let handler = Arc::new(MockEffectHandler::new("secret_handler").with_output(
            "out",
            serde_json::json!({"done": true}),
            serde_json::json!({"status": "ok"}),
        ));
        let handler_ref = handler.clone();

        service
            .register_effect_handler("secret_handler", handler)
            .unwrap();
        service.initialize(net).await.unwrap();
        service
            .create_token(
                input_id,
                TokenColor::Data(serde_json::json!({"query": "test"})),
            )
            .await
            .unwrap();

        let result = service.evaluate_until_quiescent(10).await.unwrap();
        assert_eq!(result.steps_executed, 1);

        // Handler received resolved config
        let received = handler_ref.last_execute_input.read().unwrap();
        let config = received.as_ref().unwrap().config.as_ref().unwrap();
        assert_eq!(config["auth"], "resolved_api_key");
        assert_eq!(config["url"], "https://example.com");
    }

    #[tokio::test]
    async fn test_missing_secret_fails_transition() {
        let service = create_test_service();
        // Empty store — all lookups return NotFound
        service.set_secret_store(Arc::new(TestSecretStore(HashMap::new())));

        let effect_config = serde_json::json!({"token": "{{secret:MISSING}}"});
        let (net, input_id, _output_id, _transition_id) =
            create_effect_net("fail_handler", Some(effect_config));

        let handler = Arc::new(MockEffectHandler::new("fail_handler").with_output(
            "out",
            serde_json::json!({}),
            serde_json::json!({}),
        ));
        let handler_ref = handler.clone();

        service
            .register_effect_handler("fail_handler", handler)
            .unwrap();
        service.initialize(net).await.unwrap();
        service
            .create_token(input_id, TokenColor::Data(serde_json::json!({"x": 1})))
            .await
            .unwrap();

        let result = service.evaluate_until_quiescent(10).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            ServiceError::SecretResolutionFailed { .. } => {}
            other => panic!("Expected SecretResolutionFailed, got {:?}", other),
        }

        // Handler should NOT have been called
        assert_eq!(handler_ref.execute_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_no_secret_store_passes_config_unchanged() {
        let service = create_test_service();
        // No secret store set — default None

        let effect_config = serde_json::json!({"url": "{{secret:KEY}}"});
        let (net, input_id, _output_id, _transition_id) =
            create_effect_net("passthrough_handler", Some(effect_config));

        let handler = Arc::new(MockEffectHandler::new("passthrough_handler").with_output(
            "out",
            serde_json::json!({"done": true}),
            serde_json::json!({}),
        ));
        let handler_ref = handler.clone();

        service
            .register_effect_handler("passthrough_handler", handler)
            .unwrap();
        service.initialize(net).await.unwrap();
        service
            .create_token(input_id, TokenColor::Data(serde_json::json!({"x": 1})))
            .await
            .unwrap();

        let result = service.evaluate_until_quiescent(10).await.unwrap();
        assert_eq!(result.steps_executed, 1);

        // Handler received unresolved config (refs passed through as-is)
        let received = handler_ref.last_execute_input.read().unwrap();
        let config = received.as_ref().unwrap().config.as_ref().unwrap();
        assert_eq!(config["url"], "{{secret:KEY}}");
    }

    // ========================================================================
    // dedup_id idempotency tests
    // ========================================================================

    fn dedup_net() -> (PetriNet, PlaceId) {
        let mut net = PetriNet::new();
        let place = Place::internal("inbox");
        let pid = place.id.clone();
        net.add_place(place);
        (net, pid)
    }

    async fn count_token_created<E: EventRepository>(repo: &E) -> usize {
        repo.all_events()
            .await
            .iter()
            .filter(|e| matches!(e.event, DomainEvent::TokenCreated { .. }))
            .count()
    }

    #[tokio::test]
    async fn test_duplicate_dedup_id_returns_existing_event() {
        let service = create_test_service();
        let (net, pid) = dedup_net();
        service.initialize(net).await.unwrap();

        let first = service
            .create_token_with_meta(
                pid.clone(),
                TokenColor::Unit,
                None,
                Some("lineage:1".to_string()),
                Some("slurm:job-7:completed".to_string()),
            )
            .await
            .unwrap();

        let second = service
            .create_token_with_meta(
                pid.clone(),
                TokenColor::Unit,
                None,
                Some("lineage:1".to_string()),
                Some("slurm:job-7:completed".to_string()),
            )
            .await
            .unwrap();

        assert_eq!(first.sequence, second.sequence);
        assert_eq!(first.hash, second.hash);
        assert_eq!(count_token_created(service.events.as_ref()).await, 1);
    }

    #[tokio::test]
    async fn test_same_signal_key_different_dedup_ids_both_create() {
        // The streaming case: a sig_metric place receives many tokens for one
        // execution, all sharing the same `signal_key`. Each emit has its own
        // unique `dedup_id`, so all tokens are created.
        let service = create_test_service();
        let (net, pid) = dedup_net();
        service.initialize(net).await.unwrap();

        for i in 0..3 {
            service
                .create_token_with_meta(
                    pid.clone(),
                    TokenColor::Data(serde_json::json!({"i": i})),
                    None,
                    Some("exec-42".to_string()),
                    Some(format!("exec-42-event-{}", i)),
                )
                .await
                .unwrap();
        }

        assert_eq!(count_token_created(service.events.as_ref()).await, 3);
    }

    #[tokio::test]
    async fn test_none_dedup_id_skips_dedup() {
        // Streaming events with None dedup_id always create new tokens.
        let service = create_test_service();
        let (net, pid) = dedup_net();
        service.initialize(net).await.unwrap();

        service
            .create_token_with_meta(
                pid.clone(),
                TokenColor::Unit,
                None,
                Some("k".to_string()),
                None,
            )
            .await
            .unwrap();
        service
            .create_token_with_meta(
                pid.clone(),
                TokenColor::Unit,
                None,
                Some("k".to_string()),
                None,
            )
            .await
            .unwrap();

        assert_eq!(count_token_created(service.events.as_ref()).await, 2);
    }

    #[tokio::test]
    async fn test_empty_dedup_id_skips_dedup() {
        let service = create_test_service();
        let (net, pid) = dedup_net();
        service.initialize(net).await.unwrap();

        service
            .create_token_with_meta(
                pid.clone(),
                TokenColor::Unit,
                None,
                None,
                Some(String::new()),
            )
            .await
            .unwrap();
        service
            .create_token_with_meta(
                pid.clone(),
                TokenColor::Unit,
                None,
                None,
                Some(String::new()),
            )
            .await
            .unwrap();

        assert_eq!(count_token_created(service.events.as_ref()).await, 2);
    }

    #[tokio::test]
    async fn test_same_dedup_id_different_place_creates_new_event() {
        // dedup_id is scoped to (place, id). Same id at different places is fine.
        let service = create_test_service();
        let mut net = PetriNet::new();
        let p1 = Place::internal("a");
        let p2 = Place::internal("b");
        let pid1 = p1.id.clone();
        let pid2 = p2.id.clone();
        net.add_place(p1);
        net.add_place(p2);
        service.initialize(net).await.unwrap();

        service
            .create_token_with_meta(pid1, TokenColor::Unit, None, None, Some("dup".to_string()))
            .await
            .unwrap();
        service
            .create_token_with_meta(pid2, TokenColor::Unit, None, None, Some("dup".to_string()))
            .await
            .unwrap();

        assert_eq!(count_token_created(service.events.as_ref()).await, 2);
    }

    #[tokio::test]
    async fn test_dedup_index_seeded_from_history() {
        let events_repo = Arc::new(TestEventRepo::new());
        let topology = Arc::new(TestTopologyRepo::new());
        let projection = Arc::new(TestStateProjection::new());

        let (net, pid) = dedup_net();
        topology.set_topology(net);

        let token = petri_domain::Token::new(TokenColor::Unit);
        let historical = events_repo
            .append(DomainEvent::TokenCreated {
                token,
                place_id: pid.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: Some("lineage".to_string()),
                dedup_id: Some("hist:1".to_string()),
            })
            .await
            .unwrap();

        let service = PetriNetService::new(events_repo.clone(), topology, projection);

        let replay = service
            .create_token_with_meta(
                pid.clone(),
                TokenColor::Unit,
                None,
                Some("lineage".to_string()),
                Some("hist:1".to_string()),
            )
            .await
            .unwrap();

        assert_eq!(replay.sequence, historical.sequence);
        assert_eq!(replay.hash, historical.hash);
        assert_eq!(count_token_created(events_repo.as_ref()).await, 1);
    }
}
