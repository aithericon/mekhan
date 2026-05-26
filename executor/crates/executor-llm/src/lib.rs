pub mod adapters;
pub mod backend;
pub mod config;
pub mod hardware_probe;
pub mod heartbeat;
pub mod ollama_subprocess;
pub mod pool_boot;
pub mod pool_listener;
pub mod port;
pub mod register;

pub use backend::LlmBackend;
pub use config::{LlmConfig, Provider};
pub use hardware_probe::{probe_hardware, HardwareAdvertisement};
pub use heartbeat::{heartbeat_loop, probe_loaded_models, HeartbeatConfig};
pub use ollama_subprocess::{OllamaSubprocess, OllamaSubprocessConfig};
pub use pool_boot::{register_as_pool, PoolBootConfig, PoolBootHandle};
pub use pool_listener::spawn_pool_listener;
pub use port::{
    CompletionPort, CompletionRequest, CompletionResponse, ImageData, LlmError,
};
pub use register::{
    build_register_request, default_pool_name, default_pool_tenant_id, default_requester_role,
    engine_caps_for_hardware, mint_register_jwt, register_on_boot, RegisterRequest,
    RegisterResponse,
};
