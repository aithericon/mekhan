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
        }
    }

    fn invalid_config_spec(&self) -> ExecutionSpec {
        ExecutionSpec {
            backend: "llm".into(),
            inputs: vec![],
            outputs: vec![],
            config: serde_json::json!({ "bad": "config" }),
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
        }
    }
}

aithericon_executor_test_harness::llm_conformance_tests!(llm_ollama, LlmTestKitImpl::new().await);
