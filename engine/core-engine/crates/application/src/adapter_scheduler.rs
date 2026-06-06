//! Adapter Scheduler - executes mock adapters when tokens arrive at trigger places.
//!
//! The scheduler watches for TokenCreated events and, after a configurable latency,
//! evaluates the adapter's Rhai logic to inject signal tokens into target places.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use petri_domain::{
    AdapterLogic, MockAdapterConfig, PlaceId, RegisteredAdapter, TokenColor, TokenId,
};
use rhai::Dynamic;
use serde_json::Value as JsonValue;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::rhai_runtime::RhaiRuntime;
use crate::ServiceError;

/// Result of adapter logic evaluation.
#[derive(Clone, Debug)]
pub struct AdapterResult {
    /// Target place ID (scenario string ID) to inject the token into.
    pub target_place: String,
    /// Token data to inject.
    pub data: JsonValue,
}

/// A pending adapter invocation waiting for its latency to expire.
#[derive(Clone, Debug)]
pub struct PendingAdapter {
    /// The adapter configuration.
    pub adapter: RegisteredAdapter,
    /// The token data that triggered this adapter.
    pub token_data: JsonValue,
    /// Token creation timestamp (epoch millis) for age calculation.
    pub token_created_at_ms: i64,
    /// When to execute (in milliseconds from now, for scheduling).
    pub delay_ms: u64,
}

/// Manages mock adapters and schedules their execution.
///
/// The scheduler:
/// 1. Stores registered adapters (trigger_place -> adapter config)
/// 2. Watches for token creation at trigger places
/// 3. After latency delay, evaluates Rhai logic
/// 4. Injects result tokens into target places
pub struct AdapterScheduler {
    /// Shared Rhai runtime with adapter functions (random, timestamp).
    runtime: RhaiRuntime,
    /// Registered adapters keyed by trigger place ID.
    adapters: RwLock<HashMap<PlaceId, Vec<RegisteredAdapter>>>,
}

impl Default for AdapterScheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl AdapterScheduler {
    /// Create a new adapter scheduler.
    pub fn new() -> Self {
        Self {
            runtime: RhaiRuntime::with_adapter_functions(),
            adapters: RwLock::new(HashMap::new()),
        }
    }

    /// Clear all registered adapters (called when loading a new scenario).
    pub fn clear(&self) {
        self.adapters.write().unwrap().clear();
    }

    /// Register adapters from a scenario.
    ///
    /// The `configs` are the raw MockAdapterConfigs from the scenario JSON.
    /// The `place_mapping` maps scenario string IDs to internal PlaceIds.
    pub fn register_adapters(
        &self,
        configs: &[MockAdapterConfig],
        place_mapping: &HashMap<String, PlaceId>,
    ) {
        let mut adapters = self.adapters.write().unwrap();
        adapters.clear();

        for config in configs {
            // Resolve trigger_place_id to PlaceId
            if let Some(trigger_id) = place_mapping.get(&config.trigger_place_id) {
                let registered = RegisteredAdapter {
                    name: config.name.clone(),
                    trigger_place_id: trigger_id.clone(),
                    latency_ms: config.latency_ms,
                    logic: config.logic.clone(),
                    check_token_exists: config.check_token_exists,
                };

                adapters
                    .entry(trigger_id.clone())
                    .or_default()
                    .push(registered);

                info!(
                    "Registered adapter '{}' for place {} ({}){}",
                    config.name,
                    config.trigger_place_id,
                    trigger_id,
                    if config.check_token_exists {
                        " [check_token_exists]"
                    } else {
                        ""
                    }
                );
            } else {
                warn!(
                    "Adapter '{}' references unknown place: {}",
                    config.name, config.trigger_place_id
                );
            }
        }
    }

    /// Check if a place has any adapters registered.
    pub fn has_adapters(&self, place_id: &PlaceId) -> bool {
        self.adapters.read().unwrap().contains_key(place_id)
    }

    /// Get adapters for a specific trigger place.
    pub fn get_adapters(&self, place_id: &PlaceId) -> Vec<RegisteredAdapter> {
        self.adapters
            .read()
            .unwrap()
            .get(place_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Evaluate an adapter's Rhai logic with the given token data.
    ///
    /// The script receives:
    /// - `token`: the triggering token's color data
    /// - `token_created_at`: epoch millis when the token was created (for age calculation)
    ///
    /// To compute token age: `let age_ms = timestamp() - token_created_at;`
    ///
    /// It must return a map with `target_place` (string) and `data` (any).
    pub fn evaluate_adapter(
        &self,
        adapter: &RegisteredAdapter,
        token_data: &JsonValue,
        token_created_at_ms: i64,
    ) -> Result<AdapterResult, ServiceError> {
        // Only support Rhai for now
        let source = match &adapter.logic {
            AdapterLogic::Rhai { source } => source,
            AdapterLogic::JavaScript { .. } => {
                return Err(ServiceError::ScriptError {
                    script_type: "adapter".to_string(),
                    message: "JavaScript adapters are not supported by the engine. Use Rhai."
                        .to_string(),
                });
            }
            AdapterLogic::Wasm { .. } => {
                return Err(ServiceError::ScriptError {
                    script_type: "adapter".to_string(),
                    message: "Wasm adapters are not yet supported.".to_string(),
                });
            }
        };

        // Execute the script with token data in scope
        let result = self
            .runtime
            .evaluate_adapter_script(source, token_data, token_created_at_ms)
            .map_err(|e| ServiceError::ScriptError {
                script_type: "adapter".to_string(),
                message: format!("Adapter '{}' script error: {}", adapter.name, e),
            })?;

        // Extract target_place and data from result
        let target_place = result
            .get("target_place")
            .ok_or_else(|| ServiceError::ScriptError {
                script_type: "adapter".to_string(),
                message: format!("Adapter '{}' did not return target_place", adapter.name),
            })?
            .clone()
            .into_string()
            .map_err(|_| ServiceError::ScriptError {
                script_type: "adapter".to_string(),
                message: format!("Adapter '{}' target_place must be a string", adapter.name),
            })?;

        let data_dynamic = result.get("data").cloned().unwrap_or(Dynamic::UNIT);

        let data = self.runtime.dynamic_to_json(data_dynamic)?;

        Ok(AdapterResult { target_place, data })
    }

    /// Process a token creation event and schedule any triggered adapters.
    ///
    /// This method:
    /// 1. Checks if the place has any registered adapters
    /// 2. For each adapter, spawns a tokio task that:
    ///    - Waits for the latency delay
    ///    - If `check_token_exists` is true, verifies the token is still in the place
    ///    - Evaluates the Rhai logic
    ///    - Calls the callback to inject the result token
    ///
    /// The `inject_token` callback should call `service.create_token()`.
    /// The `check_token_in_place` callback should return true if the token still exists.
    ///
    /// # Arguments
    /// * `place_id` - The place where the token was created
    /// * `token_id` - The unique ID of the token that was created
    /// * `token_data` - The token's color data as JSON
    /// * `token_created_at_ms` - When the token was created (epoch millis), for age calculation
    /// * `inject_token` - Callback to inject result tokens
    /// * `check_token_in_place` - Callback to check if a token still exists in a place
    #[allow(clippy::type_complexity)]
    pub fn process_token_created(
        self: &Arc<Self>,
        place_id: &PlaceId,
        token_id: TokenId,
        token_data: JsonValue,
        token_created_at_ms: i64,
        inject_token: Arc<dyn Fn(PlaceId, TokenColor) + Send + Sync>,
        check_token_in_place: Arc<dyn Fn(&PlaceId, &TokenId) -> bool + Send + Sync>,
    ) {
        let adapters = self.get_adapters(place_id);

        if adapters.is_empty() {
            return;
        }

        debug!(
            "Processing {} adapter(s) for place {} (token {})",
            adapters.len(),
            place_id,
            token_id
        );

        for adapter in adapters {
            let token_data = token_data.clone();
            let inject = inject_token.clone();
            let check_exists = check_token_in_place.clone();
            let scheduler = Arc::clone(self);
            let adapter_name = adapter.name.clone();
            let latency_ms = adapter.latency_ms;
            let check_token_exists = adapter.check_token_exists;
            let trigger_place_id = place_id.clone();
            let trigger_token_id = token_id.clone();

            tokio::spawn(async move {
                // Wait for latency
                debug!(
                    "Adapter '{}' waiting {}ms before execution",
                    adapter_name, latency_ms
                );
                sleep(Duration::from_millis(latency_ms)).await;

                // If check_token_exists is enabled, verify the token is still in the place
                if check_token_exists {
                    if !check_exists(&trigger_place_id, &trigger_token_id) {
                        debug!(
                            "Adapter '{}' skipped: token {} no longer in place {}",
                            adapter_name, trigger_token_id, trigger_place_id
                        );
                        return;
                    }
                    debug!(
                        "Adapter '{}' token {} still in place {}, proceeding",
                        adapter_name, trigger_token_id, trigger_place_id
                    );
                }

                // Evaluate adapter logic
                match scheduler.evaluate_adapter(&adapter, &token_data, token_created_at_ms) {
                    Ok(result) => {
                        // Resolve target place ID (string IDs are the domain IDs now)
                        let target_pid = PlaceId(result.target_place.clone());
                        let color = crate::rhai_runtime::json_to_token_color(&result.data);
                        inject(target_pid, color);
                        info!(
                            "Adapter '{}' injected token into '{}'",
                            adapter_name, result.target_place
                        );
                    }
                    Err(e) => {
                        warn!("Adapter '{}' failed: {}", adapter_name, e);
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Token-in-place predicate used by the scheduler-adapter test fixtures.
    type CheckTokenInPlace = Arc<dyn Fn(&PlaceId, &TokenId) -> bool + Send + Sync>;

    #[test]
    fn test_scheduler_creation() {
        let scheduler = AdapterScheduler::new();
        assert!(scheduler.adapters.read().unwrap().is_empty());
    }

    #[test]
    fn test_register_adapters() {
        let scheduler = AdapterScheduler::new();

        let place_id = PlaceId::new();
        let mut place_mapping = HashMap::new();
        place_mapping.insert("trigger_place".to_string(), place_id.clone());

        let configs = vec![MockAdapterConfig {
            name: "Test Adapter".to_string(),
            trigger_place_id: "trigger_place".to_string(),
            latency_ms: 500,
            logic: AdapterLogic::rhai(r#"#{ target_place: "output", data: #{ value: 42 } }"#),
            check_token_exists: false,
        }];

        scheduler.register_adapters(&configs, &place_mapping);

        assert!(scheduler.has_adapters(&place_id));
        let adapters = scheduler.get_adapters(&place_id);
        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].name, "Test Adapter");
    }

    #[test]
    fn test_evaluate_adapter_simple() {
        let scheduler = AdapterScheduler::new();

        let adapter = RegisteredAdapter {
            name: "Test".to_string(),
            trigger_place_id: PlaceId::new(),
            latency_ms: 0,
            logic: AdapterLogic::rhai(
                r#"#{ target_place: "output", data: #{ value: token.x + 1 } }"#,
            ),
            check_token_exists: false,
        };

        let token_data = json!({ "x": 10 });
        let token_created_at_ms = 1700000000000_i64; // Fixed timestamp for test
        let result = scheduler
            .evaluate_adapter(&adapter, &token_data, token_created_at_ms)
            .unwrap();

        assert_eq!(result.target_place, "output");
        assert_eq!(result.data["value"], 11);
    }

    #[test]
    fn test_evaluate_adapter_with_timestamp() {
        let scheduler = AdapterScheduler::new();

        let adapter = RegisteredAdapter {
            name: "Random".to_string(),
            trigger_place_id: PlaceId::new(),
            latency_ms: 0,
            logic: AdapterLogic::rhai(
                r#"
                let rand = timestamp() % 100;
                if rand < 50 {
                    #{ target_place: "success", data: #{ result: "ok" } }
                } else {
                    #{ target_place: "failure", data: #{ result: "fail" } }
                }
            "#,
            ),
            check_token_exists: false,
        };

        let token_data = json!({});
        let token_created_at_ms = 1700000000000_i64;
        let result = scheduler
            .evaluate_adapter(&adapter, &token_data, token_created_at_ms)
            .unwrap();

        // Should return one of the two possible results
        assert!(result.target_place == "success" || result.target_place == "failure");
    }

    #[test]
    fn test_evaluate_adapter_with_token_age() {
        let scheduler = AdapterScheduler::new();

        // Test that token_created_at is available in scope for age calculation
        let adapter = RegisteredAdapter {
            name: "Age Check".to_string(),
            trigger_place_id: PlaceId::new(),
            latency_ms: 0,
            logic: AdapterLogic::rhai(
                r#"
                let age_ms = timestamp() - token_created_at;
                #{ target_place: "output", data: #{ age_ms: age_ms } }
            "#,
            ),
            check_token_exists: false,
        };

        let token_data = json!({});
        // Set created_at to a past time so we get a positive age
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let token_created_at_ms = now_ms - 5000; // 5 seconds ago

        let result = scheduler
            .evaluate_adapter(&adapter, &token_data, token_created_at_ms)
            .unwrap();
        let age = result.data["age_ms"].as_i64().unwrap();

        // Age should be at least 5000ms (might be a bit more due to execution time)
        assert!(age >= 5000, "Age should be at least 5000ms, got {}", age);
        assert!(age < 6000, "Age should be less than 6000ms, got {}", age);
    }

    #[test]
    fn test_javascript_adapter_returns_error() {
        let scheduler = AdapterScheduler::new();

        let adapter = RegisteredAdapter {
            name: "JS Adapter".to_string(),
            trigger_place_id: PlaceId::new(),
            latency_ms: 0,
            logic: AdapterLogic::js("return { target_place: 'foo' }"),
            check_token_exists: false,
        };

        let result = scheduler.evaluate_adapter(&adapter, &json!({}), 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_clear_adapters() {
        let scheduler = AdapterScheduler::new();

        let place_id = PlaceId::new();
        let mut place_mapping = HashMap::new();
        place_mapping.insert("trigger".to_string(), place_id.clone());

        let configs = vec![MockAdapterConfig {
            name: "Test".to_string(),
            trigger_place_id: "trigger".to_string(),
            latency_ms: 100,
            logic: AdapterLogic::rhai("#{ target_place: \"out\", data: () }"),
            check_token_exists: false,
        }];

        scheduler.register_adapters(&configs, &place_mapping);
        assert!(scheduler.has_adapters(&place_id));

        scheduler.clear();
        assert!(!scheduler.has_adapters(&place_id));
    }

    #[tokio::test]
    async fn test_process_token_created_triggers_adapter() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let scheduler = Arc::new(AdapterScheduler::new());

        let trigger_place_id = PlaceId::named("trigger");
        let target_place_id = PlaceId::named("output");

        let mut place_mapping = HashMap::new();
        place_mapping.insert("trigger".to_string(), trigger_place_id.clone());
        place_mapping.insert("output".to_string(), target_place_id.clone());

        // Register an adapter with zero latency for fast testing
        let configs = vec![MockAdapterConfig {
            name: "Fast Adapter".to_string(),
            trigger_place_id: "trigger".to_string(),
            latency_ms: 0, // No latency for test
            logic: AdapterLogic::rhai(r#"#{ target_place: "output", data: #{ processed: true } }"#),
            check_token_exists: false,
        }];

        scheduler.register_adapters(&configs, &place_mapping);

        // Track how many times the inject function is called
        let call_count = Arc::new(AtomicUsize::new(0));
        let captured_place_id = Arc::new(std::sync::Mutex::new(None));

        let call_count_clone = call_count.clone();
        let captured_clone = captured_place_id.clone();

        let inject_fn: Arc<dyn Fn(PlaceId, TokenColor) + Send + Sync> =
            Arc::new(move |place_id, _color| {
                call_count_clone.fetch_add(1, Ordering::SeqCst);
                *captured_clone.lock().unwrap() = Some(place_id);
            });

        // Dummy check function (always returns true)
        let check_fn: CheckTokenInPlace = Arc::new(|_, _| true);

        // Process a token creation event
        let token_data = json!({ "value": 42 });
        let token_id = TokenId::new();
        let token_created_at_ms = 1700000000000_i64;
        scheduler.process_token_created(
            &trigger_place_id,
            token_id,
            token_data,
            token_created_at_ms,
            inject_fn,
            check_fn,
        );

        // Wait a bit for the async task to complete (since latency is 0, should be nearly instant)
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Verify the inject function was called
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "Inject function should be called once"
        );

        // Verify it was called with the correct target place
        let captured = captured_place_id.lock().unwrap();
        assert_eq!(
            *captured,
            Some(target_place_id),
            "Should inject to the correct target place"
        );
    }

    #[tokio::test]
    async fn test_process_token_created_no_adapters() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let scheduler = Arc::new(AdapterScheduler::new());
        let place_id = PlaceId::new();

        // No adapters registered for this place

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let inject_fn: Arc<dyn Fn(PlaceId, TokenColor) + Send + Sync> = Arc::new(move |_, _| {
            call_count_clone.fetch_add(1, Ordering::SeqCst);
        });

        // Dummy check function
        let check_fn: CheckTokenInPlace = Arc::new(|_, _| true);

        // Process a token creation event for a place with no adapters
        let token_id = TokenId::new();
        let token_created_at_ms = 1700000000000_i64;
        scheduler.process_token_created(
            &place_id,
            token_id,
            json!({}),
            token_created_at_ms,
            inject_fn,
            check_fn,
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Verify the inject function was NOT called
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "Inject should not be called when no adapters"
        );
    }

    #[tokio::test]
    async fn test_process_token_created_with_latency() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Instant;

        let scheduler = Arc::new(AdapterScheduler::new());

        let trigger_place_id = PlaceId::named("trigger");
        let target_place_id = PlaceId::named("output");

        let mut place_mapping = HashMap::new();
        place_mapping.insert("trigger".to_string(), trigger_place_id.clone());
        place_mapping.insert("output".to_string(), target_place_id.clone());

        // Register an adapter with 100ms latency
        let configs = vec![MockAdapterConfig {
            name: "Delayed Adapter".to_string(),
            trigger_place_id: "trigger".to_string(),
            latency_ms: 100,
            logic: AdapterLogic::rhai(r#"#{ target_place: "output", data: () }"#),
            check_token_exists: false,
        }];

        scheduler.register_adapters(&configs, &place_mapping);

        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = call_count.clone();

        let inject_fn: Arc<dyn Fn(PlaceId, TokenColor) + Send + Sync> = Arc::new(move |_, _| {
            call_count_clone.fetch_add(1, Ordering::SeqCst);
        });

        // Dummy check function
        let check_fn: CheckTokenInPlace = Arc::new(|_, _| true);

        let start = Instant::now();
        let token_id = TokenId::new();
        let token_created_at_ms = 1700000000000_i64;
        scheduler.process_token_created(
            &trigger_place_id,
            token_id,
            json!({}),
            token_created_at_ms,
            inject_fn,
            check_fn,
        );

        // Immediately after, should not have been called yet
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "Should not be called immediately"
        );

        // Wait for the latency to pass plus some buffer
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        // Now it should have been called
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "Should be called after latency"
        );
        assert!(
            start.elapsed().as_millis() >= 100,
            "Should respect latency delay"
        );
    }

    #[tokio::test]
    async fn test_process_token_created_uses_token_data() {
        let scheduler = Arc::new(AdapterScheduler::new());

        let trigger_place_id = PlaceId::named("trigger");
        let target_place_id = PlaceId::named("output");

        let mut place_mapping = HashMap::new();
        place_mapping.insert("trigger".to_string(), trigger_place_id.clone());
        place_mapping.insert("output".to_string(), target_place_id.clone());

        // Register an adapter that transforms the input token data
        let configs = vec![MockAdapterConfig {
            name: "Transform Adapter".to_string(),
            trigger_place_id: "trigger".to_string(),
            latency_ms: 0,
            logic: AdapterLogic::rhai(
                r#"#{ target_place: "output", data: #{ doubled: token.value * 2 } }"#,
            ),
            check_token_exists: false,
        }];

        scheduler.register_adapters(&configs, &place_mapping);

        let captured_color = Arc::new(std::sync::Mutex::new(None));
        let captured_clone = captured_color.clone();

        let inject_fn: Arc<dyn Fn(PlaceId, TokenColor) + Send + Sync> =
            Arc::new(move |_, color| {
                *captured_clone.lock().unwrap() = Some(color);
            });

        // Dummy check function
        let check_fn: CheckTokenInPlace = Arc::new(|_, _| true);

        // Process with specific token data
        let token_data = json!({ "value": 21 });
        let token_id = TokenId::new();
        let token_created_at_ms = 1700000000000_i64;
        scheduler.process_token_created(
            &trigger_place_id,
            token_id,
            token_data,
            token_created_at_ms,
            inject_fn,
            check_fn,
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Verify the token data was transformed correctly
        let captured = captured_color.lock().unwrap();
        if let Some(TokenColor::Data(data)) = &*captured {
            assert_eq!(data["doubled"], 42, "Adapter should transform token data");
        } else {
            panic!("Expected Data token color with transformed data");
        }
    }
}
