//! `POST /v1/inference` HTTP handler for the executor-llm pool_listener.
//!
//! Sub-phase 2.3b scaffold — implementation lands in the corresponding
//! Wave 1 dispatch slice. This stub exists so dependent crates can name
//! the module before the body is wired.
//!
//! ## Wave-2.3b framing
//!
//! `pool_listener.rs` (lines 4-15) deliberately removed the legacy
//! `cloud-layer-pool-ollama` bin's `POST /v1/inference/run` and noted:
//! "a future slice can add an HTTP-bridge if needed." Sub-phase 2.3b is
//! that slice. The wire shape that ships here MUST match what
//! cloud-layer's HttpExecutorClient (mekhan/engine/core-engine/crates/
//! application/src/http_executor_client.rs) emits — both ends are
//! authored in the same wave and must round-trip on the cert harness.
//!
//! The handler wraps `OllamaAdapter` (the existing `CompletionPort` impl)
//! against the managed Ollama subprocess — no new LLM transport, only a
//! new HTTP surface in front of it.

// Implementation lands in Item 1 of sub-phase 2.3b.
