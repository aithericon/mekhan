//! `inference-router` — the OpenAI-compatible model-pool data plane.
//!
//! Library surface for the `inference-router` binary (and its tests). See
//! `docs/29-model-pool-impl-plan.md` (Router-MVP) and
//! `docs/11-inference-router.md`. **Inference never crosses the engine net.**

pub mod admission;
pub mod auth;
pub mod cancel;
pub mod config;
pub mod inventory;
pub mod metering;
pub mod metrics;
pub mod openapi;
pub mod proxy;
pub mod routing;
