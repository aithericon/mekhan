//! Effect handlers for the typed `process_phase` / `process_progress` effects.
//!
//! Unlike `process_log_message` / `process_log_metric`, these do **not**
//! reshape the payload into a stringly log/metric breadcrumb. The input token
//! carries a serialized `aithericon_executor_domain::StatusDetail` (the
//! `PhaseChanged` / `ProgressUpdated` variant) — either at the top level or
//! nested under `detail` (executor IPC signals wrap it there). The handler
//! echoes that typed payload verbatim into `effect_result` so Mekhan's
//! causality consumer can `serde_json::from_value::<StatusDetail>()` it and
//! project the whole typed variant (started_at/ended_at/current_step/
//! total_steps/Skipped/Failed all survive — no field-by-field downgrade).

use std::collections::HashMap;

use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

/// Passthrough handler that echoes a typed `StatusDetail` payload.
///
/// **Input token** (on configured input port): either the serialized
/// `StatusDetail` directly, or `{ "detail": <StatusDetail>, ... }` (executor
/// IPC signal envelope).
///
/// **Output**: passes the input token through unchanged on the output port.
/// The typed `StatusDetail` is embedded in `effect_result` for the causality
/// consumer.
pub struct ProcessStatusDetailHandler {
    input_port: String,
    output_port: String,
    name: &'static str,
}

impl ProcessStatusDetailHandler {
    pub fn new(
        input_port: impl Into<String>,
        output_port: impl Into<String>,
        name: &'static str,
    ) -> Self {
        Self {
            input_port: input_port.into(),
            output_port: output_port.into(),
            name,
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for ProcessStatusDetailHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in {} handler",
                self.input_port, self.name,
            ))
        })?;

        // The executor watcher wraps the typed `StatusDetail` under `detail`;
        // compiler-emitted control nodes place it at the top level. Prefer the
        // nested form, fall back to the whole token, so the causality
        // consumer always receives the bare serialized `StatusDetail`.
        let detail = token_data
            .get("detail")
            .cloned()
            .unwrap_or_else(|| token_data.clone());

        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), token_data.clone());

        Ok(EffectOutput {
            tokens,
            result: detail,
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        self.name
    }
}
