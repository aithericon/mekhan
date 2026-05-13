//! Effect handler for logging structured messages to the process trace.
//!
//! Emits the log payload in its `effect_result` JSON. The causality
//! consumer in Mekhan picks up these breadcrumbs by matching on the
//! `process_log_message` effect_handler_id and writes to `hpi_logs`,
//! resolving the process via the consumed/read token tags.

use std::collections::HashMap;

use serde_json::Value as JsonValue;

use crate::effect::{EffectError, EffectHandler, EffectInput, EffectOutput};

/// Effect handler that logs a structured message to the process trace.
///
/// **Input token** (on configured input port):
/// ```json
/// {
///   "level": "info",
///   "source": "bo_optimizer",
///   "message": "Convergence reached after 42 iterations",
///   "detail": { ... }
/// }
/// ```
///
/// **Output**: passes through the input token unchanged on the output port.
/// The log data is embedded in `effect_result` for the causality consumer.
pub struct ProcessLogMessageHandler {
    input_port: String,
    output_port: String,
}

impl ProcessLogMessageHandler {
    pub fn new(input_port: impl Into<String>, output_port: impl Into<String>) -> Self {
        Self {
            input_port: input_port.into(),
            output_port: output_port.into(),
        }
    }
}

#[async_trait::async_trait]
impl EffectHandler for ProcessLogMessageHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in process_log_message handler",
                self.input_port,
            ))
        })?;

        // Token shapes we need to support:
        //   A. Direct Rhai-built tokens: { level, source, message, detail }
        //   B. Executor IPC log signals: { category: "log", detail: { level, message, fields, ... } }
        //
        // The executor watcher publishes ExternalSignal payloads whose `detail`
        // field wraps the original executor event. We fall through to detail.*
        // when the top-level fields aren't present.
        let level = token_data
            .get("level")
            .and_then(|v| v.as_str())
            .or_else(|| {
                token_data
                    .get("detail")
                    .and_then(|d| d.get("level"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("info")
            .to_string();

        let source = token_data
            .get("source")
            .and_then(|v| v.as_str())
            .or_else(|| {
                token_data
                    .get("detail")
                    .and_then(|d| d.get("source"))
                    .and_then(|v| v.as_str())
            })
            .or_else(|| {
                // Executor log entries put the user-supplied source in detail.fields.source
                token_data
                    .get("detail")
                    .and_then(|d| d.get("fields"))
                    .and_then(|f| f.get("source"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("unknown")
            .to_string();

        let message = token_data
            .get("message")
            .and_then(|v| v.as_str())
            .or_else(|| {
                token_data
                    .get("detail")
                    .and_then(|d| d.get("message"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or("")
            .to_string();

        // Prefer the nested detail as-is (carries fields, event_type, etc.);
        // fall back to top-level detail for Rhai-built tokens.
        let detail = token_data.get("detail").cloned();

        // Pass through token
        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), token_data.clone());

        Ok(EffectOutput {
            tokens,
            result: serde_json::json!({
                "level": level,
                "source": source,
                "message": message,
                "detail": detail,
            }),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        "process_log_message"
    }
}
