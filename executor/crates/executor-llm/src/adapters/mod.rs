pub mod anthropic;
pub mod ollama;
pub mod openai;
/// vLLM control-plane admin client (P2 — model-pool node agent). NOT an
/// inference adapter; see [`vllm`]. Gated behind the `vllm` feature.
#[cfg(feature = "vllm")]
pub mod vllm;

/// Unified model-pool control backend (vLLM admin OR Ollama runtime) — the
/// Metal-native path for Apple Silicon. Gated behind the `vllm` (model-pool
/// node agent) feature.
#[cfg(feature = "vllm")]
pub mod model_control;

use std::sync::Arc;

use crate::config::Provider;
use crate::port::CompletionPort;

/// Factory: select the appropriate adapter for the given provider enum.
pub fn adapter_for(provider: &Provider) -> Arc<dyn CompletionPort> {
    match provider {
        Provider::OpenAi => Arc::new(openai::OpenAiAdapter),
        Provider::Anthropic => Arc::new(anthropic::AnthropicAdapter),
        Provider::Ollama => Arc::new(ollama::OllamaAdapter),
    }
}
