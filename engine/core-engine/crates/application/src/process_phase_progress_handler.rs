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

/// Extract the bare serialized `StatusDetail` from one input token.
///
/// The executor watcher wraps the typed `StatusDetail` under `detail`;
/// compiler-emitted control nodes place it at the top level. Prefer the nested
/// form, fall back to the whole token, so the causality consumer always
/// receives the bare serialized `StatusDetail`.
fn extract_status_detail(token_data: &JsonValue) -> JsonValue {
    token_data
        .get("detail")
        .cloned()
        .unwrap_or_else(|| token_data.clone())
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

        // Batch drain (Batch-cardinality input arc): the port carries a JSON
        // array of every drained token. Echo one bare `StatusDetail` per element
        // so the causality consumer projects all N in order — and produce NO
        // pass-through token (the sink only ever discarded it).
        if let Some(arr) = token_data.as_array() {
            let details: Vec<JsonValue> = arr.iter().map(extract_status_detail).collect();
            return Ok(EffectOutput {
                tokens: HashMap::new(),
                result: JsonValue::Array(details),
            });
        }

        // Single (one token per firing): pass the token through unchanged.
        let mut tokens = HashMap::new();
        tokens.insert(self.output_port.clone(), token_data.clone());

        Ok(EffectOutput {
            tokens,
            result: extract_status_detail(token_data),
        })
    }

    fn replay(&self, _input: &EffectInput, _stored_result: &JsonValue) {
        // Stateless handler — nothing to rebuild on replay.
    }

    fn name(&self) -> &str {
        self.name
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
            transition_id: petri_domain::TransitionId::named("log_phase"),
            inputs,
            config: None,
            read_inputs: HashMap::new(),
            process_step: None,
        }
    }

    #[tokio::test]
    async fn batch_array_echoes_one_status_detail_per_element_and_no_token() {
        let h = ProcessStatusDetailHandler::new("phase", "recorded", "process_phase");
        // Each token wraps the typed StatusDetail under `detail` (executor IPC
        // envelope); the handler echoes the bare detail per element.
        let out = h
            .execute(input_with(
                "phase",
                serde_json::json!([
                    { "detail": { "kind": "phase_changed", "phase_name": "fit" } },
                    { "kind": "phase_changed", "phase_name": "done" } // top-level form
                ]),
            ))
            .await
            .expect("handler ok");

        let arr = out.result.as_array().expect("array effect_result");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["phase_name"], "fit");
        assert_eq!(arr[1]["phase_name"], "done");
        assert!(out.tokens.is_empty(), "batch drain produces no sink token");
    }

    #[tokio::test]
    async fn single_token_echoes_detail_and_passes_through() {
        let h = ProcessStatusDetailHandler::new("progress", "recorded", "process_progress");
        let out = h
            .execute(input_with(
                "progress",
                serde_json::json!({ "detail": { "fraction": 0.5 }, "x": 1 }),
            ))
            .await
            .expect("handler ok");
        assert_eq!(out.result["fraction"], 0.5);
        assert_eq!(out.tokens["recorded"]["x"], 1);
    }
}
