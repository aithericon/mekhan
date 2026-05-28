//! LLM conformance tests for the llm backend.
//!
//! Uses a shared Ollama testcontainer (auto-provisioned) so tests are fully
//! self-contained — no manual `ollama serve` or model pulls needed.
//!
//! Run with:
//!   cargo test -p aithericon-executor-llm --test conformance -- --nocapture

use std::sync::Arc;

use async_trait::async_trait;

use aithericon_executor_backend::ExecutionBackend;
use aithericon_executor_domain::ExecutionSpec;
use aithericon_executor_llm::LlmBackend;
use aithericon_executor_test_harness::conformance::llm_kit::LlmTestKit;
use aithericon_executor_test_harness::ollama::{ollama_model, shared_ollama_base_url};

struct LlmTestKitImpl {
    ollama_url: String,
    model: String,
}

impl LlmTestKitImpl {
    async fn new() -> Self {
        Self {
            ollama_url: shared_ollama_base_url().await.to_string(),
            model: ollama_model().to_string(),
        }
    }
}

#[async_trait]
impl LlmTestKit for LlmTestKitImpl {
    fn backend_name(&self) -> &'static str {
        "llm"
    }

    async fn create_backend(&self) -> Result<Arc<dyn ExecutionBackend>, String> {
        Ok(Arc::new(LlmBackend::new()))
    }

    /// Probe the testcontainer Ollama by issuing a one-token chat. Some host
    /// configurations (CPU-only Docker on Apple Silicon, low-memory CI runners)
    /// start the container fine but crash the model loader at inference time
    /// (HTTP 500 "llama runner process has terminated"). Skip the conformance
    /// suite in those environments rather than failing.
    async fn skip_reason(&self) -> Option<String> {
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/api/chat", self.ollama_url))
            .json(&serde_json::json!({
                "model": self.model,
                "messages": [{"role": "user", "content": "hi"}],
                "stream": false,
                "options": {"num_predict": 1},
            }))
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .ok()?;
        if resp.status().is_success() {
            return None;
        }
        let body = resp.text().await.unwrap_or_default();
        Some(format!(
            "Ollama testcontainer can't run model {} on this host: {body}",
            self.model
        ))
    }

    fn chat_spec(&self) -> ExecutionSpec {
        ExecutionSpec {
            backend: "llm".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({
                "provider": "ollama",
                "model": self.model,
                "prompt": "Reply with exactly the word 'hello' and nothing else.",
                "base_url": self.ollama_url,
            }),
                config_ref: None,
        }
    }

    fn extract_spec(&self) -> ExecutionSpec {
        ExecutionSpec {
            backend: "llm".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({
                "provider": "ollama",
                "model": self.model,
                "prompt": "The capital of France is Paris. The population is approximately 2.1 million.",
                "system_prompt": "Extract the requested information from the text.",
                "base_url": self.ollama_url,
                "response_format": {
                    "type": "json_schema",
                    "schema": {
                        "type": "object",
                        "properties": {
                            "city": { "type": "string", "description": "The city name" },
                            "country": { "type": "string", "description": "The country name" },
                            "population": { "type": "string", "description": "The approximate population" }
                        },
                        "required": ["city", "country"]
                    }
                }
            }),
                config_ref: None,
        }
    }

    fn extract_no_schema_spec(&self) -> ExecutionSpec {
        ExecutionSpec {
            backend: "llm".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({
                "provider": "ollama",
                "model": self.model,
                "prompt": "Extract something",
                "base_url": self.ollama_url,
                "response_format": {
                    "type": "json_schema",
                    "schema": null
                }
            }),
                config_ref: None,
        }
    }

    fn invalid_config_spec(&self) -> ExecutionSpec {
        ExecutionSpec {
            backend: "llm".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({ "bad": "config" }),
            config_ref: None,
        }
    }

    fn api_error_spec(&self) -> ExecutionSpec {
        ExecutionSpec {
            backend: "llm".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({
                "provider": "ollama",
                "model": "nonexistent-model-xyz-99999",
                "prompt": "Hello",
                "base_url": self.ollama_url,
            }),
                config_ref: None,
        }
    }
}

aithericon_executor_test_harness::llm_conformance_tests!(llm_ollama, LlmTestKitImpl::new().await);
