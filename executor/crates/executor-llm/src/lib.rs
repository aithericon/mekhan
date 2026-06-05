pub mod adapters;
pub mod agent_loop;
pub mod backend;
pub mod config;
pub mod execute_handler;
pub mod hardware_probe;
pub mod heartbeat;
pub mod inference_handler;
#[cfg(feature = "kreuzberg")]
pub mod ocr_handler;
/// Wire DTOs for the model load/unload command channel (P2). Gated behind the
/// `vllm` feature (the model-pool node agent's payload contract).
#[cfg(feature = "vllm")]
pub mod model_command;
pub mod ollama_subprocess;
pub mod pool_boot;
pub mod pool_listener;
pub mod port;
pub mod register;

pub use backend::LlmBackend;
pub use config::{LlmConfig, Provider};
pub use hardware_probe::{probe_hardware, HardwareAdvertisement};
pub use heartbeat::{heartbeat_loop, probe_loaded_models, HeartbeatConfig};
pub use inference_handler::InferenceState;
// vLLM control-plane node-agent surface (P2 — model-pool). The
// `VllmAdapter::probe_loaded_models` method is DISTINCT from the
// `heartbeat::probe_loaded_models` re-exported above (the old cloud
// capability-routing one) — do not conflate them.
#[cfg(feature = "vllm")]
pub use adapters::vllm::{LoadedModel, VllmAdapter};
#[cfg(feature = "vllm")]
pub use model_command::{LoadTarget, ModelCommand};
pub use ollama_subprocess::{OllamaSubprocess, OllamaSubprocessConfig};
pub use pool_boot::{register_as_pool, PoolBootConfig, PoolBootHandle};
pub use pool_listener::{spawn_pool_listener, ToolResultsState};
pub use port::{CompletionPort, CompletionRequest, CompletionResponse, ImageData, LlmError};
pub use register::{
    build_register_request, default_pool_name, default_pool_tenant_id, default_requester_role,
    engine_caps_for_hardware, mint_register_jwt, register_on_boot, RegisterRequest,
    RegisterResponse,
};
