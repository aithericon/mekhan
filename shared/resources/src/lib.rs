//! # `aithericon-resources` — Typed workflow resource declarations
//!
//! This crate is the **declarative half** of the Resource model designed in
//! the *yes-instruct-a-subagent-indexed-locket* plan. It provides:
//!
//! - Built-in [`ResourceType`] structs that workflow authors reference by
//!   alias (`db: postgres`, `ai: openai`).
//! - A compile-time registry, populated via [`inventory`], that the service
//!   reads to (a) validate workflow-level alias declarations and (b) emit a
//!   `ResourceTypeInfo` payload for the frontend picker.
//! - Reference shapes ([`ResourceRef`], [`ResourcePin`], [`ResolvedResource`])
//!   that travel through the service launcher and the AIR pipeline.
//!
//! The crate is intentionally **infrastructure-free**: no DB, no Vault, no
//! HTTP. The resolution layer (DB lookup, ACL, audit, Vault path emission)
//! lives in `service/src/petri/resource_resolver.rs` (to be added in B.5).
//!
//! Resources compile *down to* `{{secret:resources/<id>/v<n>#<field>}}`
//! patterns understood by the existing `shared/secrets` kernel. They do not
//! replace it.
//!
//! ## Phase status
//!
//! This is **B.1+B.2 only**. The resolver (B.5), handlers (B.9), compiler
//! integration (B.6), and OAuth (B.11) build on this crate but live elsewhere.

#![forbid(unsafe_code)]

// The `#[derive(ResourceType)]` expansion references types by the absolute
// path `::aithericon_resources::…` so downstream crates Just Work. That path
// fails to resolve when the derive is invoked *inside this crate itself*
// (lib `aithericon-resources` doesn't import itself). The standard fix is
// `extern crate self as <name>`: it aliases the current crate's root to its
// own name, making the absolute path resolvable from inside.
extern crate self as aithericon_resources;

pub mod pool;
pub mod refs;
pub mod registry;
pub mod store;
pub mod types;

pub use refs::{ResolvedResource, ResourcePin, ResourceRef};
pub use registry::{all, lookup, ResourceTypeDescriptor};
pub use store::{InMemoryResourceStore, ResourceSecretStore, ResourceStoreError};
#[cfg(feature = "vault")]
pub use store::VaultResourceStore;

/// Re-export the derive macro so consumers can write
/// `use aithericon_resources::ResourceType;` and have both the trait-like
/// surface and the derive resolved from the same path.
pub use aithericon_resources_derive::ResourceType;

/// Re-exports used by the `#[derive(ResourceType)]` expansion. **Not** part
/// of the stable surface — anything in here may change without a semver bump.
/// External callers must never reach into this module by hand.
#[doc(hidden)]
pub mod __macro_support {
    pub use inventory;
    pub use schemars;
    pub use serde_json;
}
