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

/// Extract one `{ key, value }` metric record from a single input token.
///
/// Token shapes supported:
///   A. Direct Rhai-built tokens: `{ key, value }`
///   B. Executor IPC metric signals: `{ category: "metric", detail: { name, value, step, ... } }`
///
/// The executor emits metric points with `name` (not `key`), so we try both
/// paths when falling back to `detail.*`. No fabricated fallback: the
/// executor's end-of-execution `metrics_logged` summary rides the same metric
/// signal but carries no name/value — we emit an empty key so the mekhan
/// consumer's empty-key guard drops it instead of recording a spurious series.
fn extract_metric(token_data: &JsonValue) -> JsonValue {
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
        .unwrap_or("")
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

    let mut record = serde_json::json!({ "key": key, "value": value });
    // Carry the client emit timestamp (the executor `event.timestamp`, RFC3339,
    // at the top of the IPC signal payload) so the metric is recorded at its
    // emit time, not the drain/ingest time — important once a drain batches many
    // points into one firing. Rhai-built `{ key, value }` tokens carry no
    // timestamp; the causality consumer then falls back to the event time.
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
impl EffectHandler for ProcessLogMetricHandler {
    async fn execute(&self, input: EffectInput) -> Result<EffectOutput, EffectError> {
        let token_data = input.inputs.get(&self.input_port).ok_or_else(|| {
            EffectError::Fatal(format!(
                "Missing input port '{}' in process_log_metric handler",
                self.input_port,
            ))
        })?;

        // Batch drain (Batch-cardinality input arc): the port carries a JSON
        // array of every drained token. Emit one record per element so the
        // causality consumer ingests all N — and produce NO pass-through token
        // (the sink only ever discarded it), keeping the marking O(1).
        if let Some(arr) = token_data.as_array() {
            let records: Vec<JsonValue> = arr.iter().map(extract_metric).collect();
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
            result: extract_metric(token_data),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        "process_log_metric"
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
            transition_id: petri_domain::TransitionId::named("log_metric"),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    #[tokio::test]
    async fn batch_array_emits_one_record_per_element_and_no_token() {
        let h = ProcessLogMetricHandler::new("metric", "logged");
        let out = h
            .execute(input_with(
                "metric",
                serde_json::json!([
                    { "key": "loss", "value": 0.5 },
                    { "detail": { "name": "acc", "value": 0.9 } }, // detail.name fallback
                    { "key": "", "value": 0.0 } // empty key preserved (consumer drops it)
                ]),
            ))
            .await
            .expect("handler ok");

        let arr = out.result.as_array().expect("array effect_result");
        assert_eq!(arr.len(), 3, "one record per drained token");
        assert_eq!(arr[0]["key"], "loss");
        assert_eq!(arr[0]["value"], 0.5);
        assert_eq!(arr[1]["key"], "acc");
        assert_eq!(arr[1]["value"], 0.9);
        assert_eq!(arr[2]["key"], "");
        assert!(
            out.tokens.is_empty(),
            "a batch drain produces no pass-through sink token"
        );
    }

    #[tokio::test]
    async fn single_object_passthrough_unchanged() {
        let h = ProcessLogMetricHandler::new("metric", "logged");
        let out = h
            .execute(input_with("metric", serde_json::json!({ "key": "loss", "value": 1.0 })))
            .await
            .expect("handler ok");
        assert_eq!(out.result["key"], "loss");
        assert_eq!(out.result["value"], 1.0);
        // Single mode still passes the token through on the output port.
        assert_eq!(out.tokens["logged"]["key"], "loss");
    }

    #[tokio::test]
    async fn carries_client_timestamp_per_record_when_present() {
        let h = ProcessLogMetricHandler::new("metric", "logged");
        let out = h
            .execute(input_with(
                "metric",
                serde_json::json!([
                    { "key": "loss", "value": 0.5, "timestamp": "2026-06-30T12:00:00Z" },
                    { "detail": { "name": "acc", "value": 0.9, "timestamp": "2026-06-30T12:00:01Z" } },
                    { "key": "noise", "value": 1.0 } // no client ts → key omitted
                ]),
            ))
            .await
            .expect("handler ok");
        let arr = out.result.as_array().unwrap();
        assert_eq!(arr[0]["ts"], "2026-06-30T12:00:00Z");
        assert_eq!(arr[1]["ts"], "2026-06-30T12:00:01Z"); // detail.timestamp fallback
        assert!(arr[2].get("ts").is_none(), "absent client ts → no ts key");
    }
}
