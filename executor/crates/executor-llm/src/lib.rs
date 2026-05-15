pub mod adapters;
pub mod backend;
pub mod config;
pub mod hardware_probe;
pub mod ollama_subprocess;
pub mod port;

pub use backend::LlmBackend;
pub use config::{LlmConfig, Provider};
pub use hardware_probe::{probe_hardware, HardwareAdvertisement};
pub use ollama_subprocess::{OllamaSubprocess, OllamaSubprocessConfig};
pub use port::{
    CompletionPort, CompletionRequest, CompletionResponse, FinishReason, ImageData, LlmError,
    TokenUsage,
};
