//! # `aithericon-backends` — Declarative backend metadata
//!
//! The cross-crate half of the backend registry. Holds the wire-name enum
//! ([`ExecutionBackendType`]), the dispatch-mode discriminator
//! ([`DispatchMode`]), the resource-channel discriminator
//! ([`ResourceChannel`]), and a `&'static [BackendMeta]` slice listing
//! every shipped backend.
//!
//! ## What lives here vs. elsewhere
//!
//! **Here (cross-crate):** wire identity + dispatch shape. The minimum both
//! `mekhan-service` (compile-time validation, UI metadata) and
//! `aithericon-executor-service` (runtime registration) need to agree on.
//!
//! **In `mekhan-service::backends` (compile-time):** the per-backend
//! validators, ref scanners, default editor configs, and the `BackendDecl`
//! wrapper that adds those fn pointers to a [`BackendMeta`].
//!
//! **In `executor-worker` (runtime):** the trait-based `ExecutionBackend`
//! impls — `prepare`, `execute`, artifact emission. The executor reads the
//! [`DispatchMode::ExecutorJob`] filter off this crate to know which
//! descriptors it owns.
//!
//! The split mirrors `shared/resources`: declarative half here, runtime
//! and compile-time halves in their respective binaries.

#![forbid(unsafe_code)]

mod registry;
mod types;

pub use registry::{
    lookup, BackendMeta, BACKENDS, CATALOGUE_QUERY_META, DOCKER_META, FILE_OPS_META, HTTP_META,
    KREUZBERG_META, LLM_META, PROCESS_META, PYTHON_META, SMTP_META,
};
pub use types::{DispatchMode, ExecutionBackendType, ResourceChannel};
