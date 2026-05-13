//! Timer effect handler for durable delays.
//!
//! Integrates the durable timer (Clockmaster) into the Petri engine.
//! Transitions can fire a "timer_schedule" effect to wait for a duration.

use std::collections::HashMap;
use std::sync::Arc;

use petri_domain::timer::{TimerCancelRequest, TimerClient, TimerScheduleRequest};
use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

/// Effect handler that schedules a durable timer.
///
/// Consumes an input token with delay info and schedules it via `TimerClient`.
pub struct TimerScheduleHandler {
    client: Arc<dyn TimerClient>,
    input_port: String,
    output_port: String,
    net_id: String,
    /// Optional override for delay (if not in token)
    static_delay_ms: Option<u64>,
    /// Optional override for target place (if not in token)
    static_target_place_id: Option<String>,
}

impl TimerScheduleHandler {
    pub fn new(
        client: Arc<dyn TimerClient>,
        net_id: impl Into<String>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            client,
            net_id: net_id.into(),
            input_port: input_port.into(),
            output_port: output_port.into(),
            static_delay_ms: None,
            static_target_place_id: None,
        }
    }

    pub fn with_static_config(mut self, delay_ms: u64, target_place_id: impl Into<String>) -> Self {
        self.static_delay_ms = Some(delay_ms);
        self.static_target_place_id = Some(target_place_id.into());
        self
    }
}

#[async_trait::async_trait]
impl EffectHandler for TimerScheduleHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let timer_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in timer schedule handler",
                self.input_port
            ))
        })?;

        // Support both "timer" (nested) and top-level data
        let nested_timer = timer_data.get("timer");
        let source = nested_timer.unwrap_or(timer_data);

        let delay_ms = self
            .static_delay_ms
            .or_else(|| source.get("delay_ms").and_then(|v| v.as_u64()))
            .ok_or_else(|| EffectError::Fatal("Missing delay_ms in timer input".to_string()))?;

        let target_place_id = self
            .static_target_place_id
            .as_deref()
            .or_else(|| source.get("target_place_id").and_then(|v| v.as_str()))
            .ok_or_else(|| {
                EffectError::Fatal("Missing target_place_id in timer input".to_string())
            })?;

        let payload = source.get("payload").cloned().unwrap_or(JsonValue::Null);
        let correlation_id = uuid::Uuid::new_v4();

        self.client
            .schedule(TimerScheduleRequest {
                net_id: self.net_id.clone(),
                place_id: target_place_id.to_string(),
                correlation_id,
                delay_ms,
                payload: payload.clone(),
            })
            .await
            .map_err(|e| EffectError::ExecutionFailed(e.to_string()))?;

        // Build output token: confirm scheduling.
        let mut output_data = timer_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            obj.insert("scheduled".to_string(), JsonValue::Bool(true));
            obj.insert(
                "timer_correlation_id".to_string(),
                JsonValue::String(correlation_id.to_string()),
            );
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "delay_ms": delay_ms,
                "target": target_place_id,
                "signal_key": correlation_id,
            }),
        })
    }

    fn name(&self) -> &str {
        "timer_schedule"
    }

    fn port_schemas(&self) -> Option<crate::effect::EffectPortSchemas> {
        Some(crate::effect::EffectPortSchemas {
            inputs: HashMap::from([(
                self.input_port.clone(),
                "#/definitions/TimerInput".into(),
            )]),
            outputs: HashMap::from([(
                self.output_port.clone(),
                "#/definitions/TimerScheduled".into(),
            )]),
        })
    }
}

/// Effect handler that cancels a previously scheduled timer.
///
/// Consumes an input token with timer_correlation_id and target_place_id,
/// then deletes the timer from the KV store.
pub struct TimerCancelHandler {
    client: Arc<dyn TimerClient>,
    input_port: String,
    output_port: String,
    net_id: String,
}

impl TimerCancelHandler {
    pub fn new(
        client: Arc<dyn TimerClient>,
        net_id: impl Into<String>,
        input_port: impl Into<String>,
        output_port: impl Into<String>,
    ) -> Self {
        Self {
            client,
            net_id: net_id.into(),
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for TimerCancelHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let cancel_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in timer cancel handler",
                self.input_port
            ))
        })?;

        // Extract correlation_id and target_place_id from the input
        let correlation_id_str = cancel_data
            .get("timer_correlation_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal("Missing timer_correlation_id in cancel input".to_string())
            })?;

        let correlation_id = uuid::Uuid::parse_str(correlation_id_str)
            .map_err(|e| EffectError::Fatal(format!("Invalid correlation_id: {}", e)))?;

        let target_place_id = cancel_data
            .get("target_place_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                EffectError::Fatal("Missing target_place_id in cancel input".to_string())
            })?;

        let cancelled = self
            .client
            .cancel(TimerCancelRequest {
                net_id: self.net_id.clone(),
                place_id: target_place_id.to_string(),
                correlation_id,
            })
            .await
            .map_err(|e| EffectError::ExecutionFailed(e.to_string()))?;

        // Build output token with cancellation result
        let mut output_data = cancel_data.clone();
        if let Some(obj) = output_data.as_object_mut() {
            obj.insert("cancelled".to_string(), JsonValue::Bool(cancelled));
        }

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), output_data);

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "cancelled": cancelled,
                "correlation_id": correlation_id,
            }),
        })
    }

    fn name(&self) -> &str {
        "timer_cancel"
    }

    fn port_schemas(&self) -> Option<crate::effect::EffectPortSchemas> {
        Some(crate::effect::EffectPortSchemas {
            inputs: HashMap::from([(
                self.input_port.clone(),
                "#/definitions/TimerCancelInput".into(),
            )]),
            outputs: HashMap::from([(
                self.output_port.clone(),
                "#/definitions/TimerCancelled".into(),
            )]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::EffectHandler;

    #[test]
    fn test_schedule_handler_port_schemas() {
        let client = Arc::new(MockTimerClient);
        let handler = TimerScheduleHandler::new(client, "net-1", "timer", "scheduled");
        let schemas = handler.port_schemas().expect("should declare port schemas");
        assert_eq!(
            schemas.inputs.get("timer").unwrap(),
            "#/definitions/TimerInput"
        );
        assert_eq!(
            schemas.outputs.get("scheduled").unwrap(),
            "#/definitions/TimerScheduled"
        );
    }

    #[test]
    fn test_cancel_handler_port_schemas() {
        let client = Arc::new(MockTimerClient);
        let handler = TimerCancelHandler::new(client, "net-1", "timer", "cancelled");
        let schemas = handler.port_schemas().expect("should declare port schemas");
        assert_eq!(
            schemas.inputs.get("timer").unwrap(),
            "#/definitions/TimerCancelInput"
        );
        assert_eq!(
            schemas.outputs.get("cancelled").unwrap(),
            "#/definitions/TimerCancelled"
        );
    }

    /// Minimal mock timer client for tests.
    struct MockTimerClient;

    #[async_trait::async_trait]
    impl TimerClient for MockTimerClient {
        async fn schedule(
            &self,
            _request: TimerScheduleRequest,
        ) -> Result<(), petri_domain::timer::TimerError> {
            Ok(())
        }
        async fn cancel(
            &self,
            _request: TimerCancelRequest,
        ) -> Result<bool, petri_domain::timer::TimerError> {
            Ok(true)
        }
        fn name(&self) -> &str {
            "mock-timer"
        }
    }
}
