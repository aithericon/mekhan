//! Effect handler for logging numeric metrics to the process trace.
//!
//! Emits the metric payload in its `effect_result` JSON. The causality
//! consumer in Mekhan picks up these breadcrumbs by matching on the
//! `process_log_metric` effect_handler_id and writes to `hpi_metrics`,
//! resolving the process via the consumed/read token tags.

use std::collections::HashMap;

use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

/// Effect handler that logs a numeric metric to the process trace.
///
/// **Input token** (on configured input port):
/// ```json
/// {
///   "key": "acquisition_value",
///   "value": 3.14,
///   ...
/// }
/// ```
/// or nested under `detail`:
/// ```json
/// {
///   "detail": { "key": "loss", "value": 0.042 },
///   ...
/// }
/// ```
///
/// **Output**: passes through the input token unchanged on the output port.
/// The metric data is embedded in `effect_result` for the causality consumer.
pub struct ProcessLogMetricHandler {
    input_port: String,
    output_port: String,
}

impl ProcessLogMetricHandler {
    pub fn new(input_port: impl Into<String>, output_port: impl Into<String>) -> Self {
        Self {
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for ProcessLogMetricHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in process_log_metric handler",
                self.input_port,
            ))
        })?;

        // Token shapes we need to support:
        //   A. Direct Rhai-built tokens: { key, value }
        //   B. Executor IPC metric signals: { category: "metric", detail: { name, value, step, ... } }
        //
        // The executor emits metric points with `name` (not `key`), so we try
        // both paths when falling back to detail.*
        let key = token_data
            .get("key")
            .and_then(|v| v.as_str())
            .or_else(|| {
                token_data
                    .get("detail")
                    .and_then(|d| d.get("key"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| {
                token_data
                    .get("detail")
                    .and_then(|d| d.get("name"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("unknown")
            .to_string();

        let value = token_data
            .get("value")
            .and_then(|v| v.as_f64())
            .or_else(|| {
                token_data
                    .get("detail")
                    .and_then(|d| d.get("value"))
                    .and_then(|v| v.as_f64())
            })
            .unwrap_or(0.0);

        // Pass through token
        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), token_data.clone());

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "key": key,
                "value": value,
            }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        "process_log_metric"
    }
}
