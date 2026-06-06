//! Engine-side mirror DTOs for the generic `/v1/execute` pool surface.
//!
//! Stage 1 of the generic-execute-surface work. The engine and the executor
//! pools are SEPARATE cargo workspaces with no shared crate across the
//! boundary, so the wire contract lives as a pair of mirror DTOs — one on
//! each side — that MUST stay byte-for-byte compatible.
//!
//! ## Wire contract (authoritative — mirror these EXACT field names)
//!
//! ```text
//! ExecuteRequest  { backend: String, task_kind: String,
//!                   model: Option<String>, config: Value, input: Value }
//! ExecuteResponse { outputs: Map<String, Value> }
//! ```
//!
//! ## Directionality
//!
//! The engine is the *client* of `/v1/execute`: it **serializes**
//! [`ExecuteRequest`] into the POST body and **deserializes**
//! [`ExecuteResponse`] from the pool's reply. The pool side
//! (`executor-domain::execute_contract`) does the reverse. Both directions are
//! derived here for symmetry and to let the round-trip test exercise the full
//! loop, but only `ExecuteRequest: Serialize` and `ExecuteResponse: Deserialize`
//! are load-bearing on the engine.
//!
//! ## Drift guard
//!
//! The `#[cfg(test)]` round-trip test below pins an identical fixed JSON
//! literal that also appears verbatim in the pool-side module. If either side
//! renames a field or changes a shape, the matching test fails on the other
//! side, surfacing the drift before it reaches the wire.
//!
//! `model` fails closed AT THE POOL (the LLM pool 400s on an empty model), so
//! the engine can carry `model` as `Option<String>` without weakening the
//! no-default-model rule.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Generic `/v1/execute` request body (engine → pool).
///
/// The engine serializes this from the enriched effect_config + input token.
/// `config` carries task-specific knobs (system prompt, tool catalogue, …) and
/// `input` carries the task payload (prompt, images, file bytes, …) — both as
/// opaque JSON so the contract stays task-agnostic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecuteRequest {
    /// Pool backend selector (e.g. `"llm"`, `"surya"`). Injected into the
    /// enriched effect_config by cloud-layer cap-routing (`pool_backend`).
    pub backend: String,
    /// Task discriminator the pool uses to shape the call (e.g. `"Chat"`,
    /// `"Vision"`, `"Agent"`, `"StructuredOutput"`, `"Ocr"`).
    pub task_kind: String,
    /// Model identifier. `None` is permitted on the wire; the pool fails closed
    /// when a model is required and absent (no-default-model).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Task-specific configuration (opaque to the contract).
    pub config: Value,
    /// Task payload (opaque to the contract).
    pub input: Value,
}

/// Generic `/v1/execute` response body (pool → engine).
///
/// The engine deserializes this and nests `outputs` under the output token's
/// `detail.outputs` so downstream Rhai (`outputs_of` reads `tok.detail.outputs`
/// first) can address pool outputs by canonical key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecuteResponse {
    /// Canonical, pool-defined output map (e.g. LLM `{output, model,
    /// finish_reason, usage, structured_output}`; Surya `{full_text, words,
    /// pages, ocr_text, page_count, engine, mime_type}`).
    pub outputs: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------
    // FIXED JSON SAMPLES — IDENTICAL LITERALS on the pool side
    // (`executor-domain::execute_contract`). Do NOT edit one without the
    // other; the pair is the cross-workspace drift guard.
    // ---------------------------------------------------------------------

    const REQUEST_SAMPLE: &str = r#"{"backend":"llm","task_kind":"Chat","model":"qwen3.5:9b","config":{"system_prompt":"You are a clinical assistant.","temperature":0.2},"input":{"prompt":"Summarize the chart.","images":[]}}"#;

    const RESPONSE_SAMPLE: &str = r#"{"outputs":{"output":"Summary text.","model":"qwen3.5:9b","finish_reason":"stop","usage":{"prompt_tokens":12,"completion_tokens":3}}}"#;

    #[test]
    fn execute_request_roundtrip_against_fixed_sample() {
        // Deserialize the fixed sample, re-serialize, and assert structural
        // equality with the literal. Equality is asserted at the `Value` level
        // (not byte level) because nested `config`/`input` objects are
        // serde_json::Value, whose keys re-sort on serialization without the
        // `preserve_order` feature; the struct fields themselves keep
        // declaration order. This still catches any field rename / shape change
        // on either side — the cross-workspace drift guard.
        let req: ExecuteRequest = serde_json::from_str(REQUEST_SAMPLE).unwrap();
        assert_eq!(req.backend, "llm");
        assert_eq!(req.task_kind, "Chat");
        assert_eq!(req.model.as_deref(), Some("qwen3.5:9b"));
        assert_eq!(req.config["system_prompt"], "You are a clinical assistant.");
        assert_eq!(req.input["prompt"], "Summarize the chart.");

        let reserialized: Value =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        let expected: Value = serde_json::from_str(REQUEST_SAMPLE).unwrap();
        assert_eq!(reserialized, expected);
    }

    #[test]
    fn execute_request_model_none_is_omitted() {
        let req = ExecuteRequest {
            backend: "llm".into(),
            task_kind: "Chat".into(),
            model: None,
            config: serde_json::json!({}),
            input: serde_json::json!({}),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("model"),
            "None model must not appear on the wire"
        );
    }

    #[test]
    fn execute_response_roundtrip_against_fixed_sample() {
        let resp: ExecuteResponse = serde_json::from_str(RESPONSE_SAMPLE).unwrap();
        assert_eq!(resp.outputs["output"], "Summary text.");
        assert_eq!(resp.outputs["model"], "qwen3.5:9b");
        assert_eq!(resp.outputs["finish_reason"], "stop");

        let reserialized: Value =
            serde_json::from_str(&serde_json::to_string(&resp).unwrap()).unwrap();
        let expected: Value = serde_json::from_str(RESPONSE_SAMPLE).unwrap();
        assert_eq!(reserialized, expected);
    }
}
