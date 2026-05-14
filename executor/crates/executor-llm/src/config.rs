//! Re-export of wire-format LLM config types from the shared backend-configs crate.
//!
//! Types live in `aithericon-executor-backend-configs::llm` so the mekhan
//! compiler and the executor share a single source of truth for the JSON
//! shape that crosses the wire.

pub use aithericon_executor_backend_configs::llm::{
    ChatMessage, ImageInput, LlmConfig, Provider, ResponseFormat, Role,
};
