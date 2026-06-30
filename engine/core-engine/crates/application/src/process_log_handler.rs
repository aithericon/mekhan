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

/// Extract one `{ level, source, message, detail }` log record from a token.
///
/// Token shapes supported:
///   A. Direct Rhai-built tokens: `{ level, source, message, detail }`
///   B. Executor IPC log signals: `{ category: "log", detail: { level, message, fields, ... } }`
///
/// The executor watcher publishes ExternalSignal payloads whose `detail` field
/// wraps the original executor event, so each field falls through to `detail.*`
/// when the top-level form is absent.
fn extract_log(token_data: &JsonValue) -> JsonValue {
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

    let detail = token_data.get("detail").cloned();

    let mut record = serde_json::json!({
        "level": level,
        "source": source,
        "message": message,
        "detail": detail,
    });
    // Carry the client emit timestamp (the executor `event.timestamp`, RFC3339,
    // at the top of the IPC signal payload) so the log line is recorded at its
    // emit time, not the drain/ingest time. Rhai-built tokens carry none; the
    // causality consumer then falls back to the event time.
    if let Some(ts) = client_ts(token_data) {
        record["ts"] = JsonValue::String(ts);
    }
    record
}

/// The client emit timestamp (RFC3339) from a telemetry token, if present:
/// top-level `timestamp` (the IPC signal envelope) or nested `detail.timestamp`.
fn client_ts(token_data: &JsonValue) -> Option<String> {
    token_data
        .get("timestamp")
        .and_then(|v| v.as_str())
        .or_else(|| {
            token_data
                .get("detail")
                .and_then(|d| d.get("timestamp"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.to_string())
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

        // Batch drain (Batch-cardinality input arc): the port carries a JSON
        // array of every drained token. Emit one record per element so the
        // causality consumer ingests all N — and produce NO pass-through token
        // (the sink only ever discarded it), keeping the marking O(1).
        if let Some(arr) = token_data.as_array() {
            let records: Vec<JsonValue> = arr.iter().map(extract_log).collect();
            return Ok(EffectOutput {
                tokens: HashMap::new(),
                result: JsonValue::Array(records),
            });
        }

        // Single (one token per firing): pass the token through unchanged.
        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), token_data.clone());

        Ok(EffectOutput {
            tokens,
            result: extract_log(token_data),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        "process_log_message"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::EffectHandler;

    fn input_with(port: &str, value: JsonValue) -> EffectInput {
        let mut inputs = HashMap::new();
        inputs.insert(port.to_string(), value);
        EffectInput {
            transition_id: petri_domain::TransitionId::named("log_message"),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    #[tokio::test]
    async fn batch_array_emits_one_log_per_element_and_no_token() {
        let h = ProcessLogMessageHandler::new("message", "logged");
        let out = h
            .execute(input_with(
                "message",
                serde_json::json!([
                    { "level": "info", "source": "bo", "message": "start" },
                    { "detail": { "level": "warn", "message": "slow", "fields": { "source": "exec" } } }
                ]),
            ))
            .await
            .expect("handler ok");

        let arr = out.result.as_array().expect("array effect_result");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["level"], "info");
        assert_eq!(arr[0]["source"], "bo");
        assert_eq!(arr[0]["message"], "start");
        // detail.* fallbacks for the executor IPC envelope shape.
        assert_eq!(arr[1]["level"], "warn");
        assert_eq!(arr[1]["message"], "slow");
        assert_eq!(arr[1]["source"], "exec");
        assert!(out.tokens.is_empty(), "batch drain produces no sink token");
    }

    #[tokio::test]
    async fn single_object_passthrough_unchanged() {
        let h = ProcessLogMessageHandler::new("message", "logged");
        let out = h
            .execute(input_with(
                "message",
                serde_json::json!({ "level": "error", "message": "boom" }),
            ))
            .await
            .expect("handler ok");
        assert_eq!(out.result["level"], "error");
        assert_eq!(out.result["message"], "boom");
        assert_eq!(out.tokens["logged"]["message"], "boom");
    }
}
