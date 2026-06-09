//! Unified Data-browser read-model (docs/32 §4.1).
//!
//! Composes the `catalogue` (logical) and `inventory` (physical) repositories
//! into one view keyed on the logical entry, with physical copies nested and
//! file-server names resolved — the backend for the consolidated Data page.

pub mod handlers;
pub mod model;
pub mod queries;
pub mod serve;
