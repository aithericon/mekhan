//! NATS subject naming conventions.
//!
//! The canonical definitions live in [`petri_api_types::subjects`] so that
//! non-engine consumers (e.g. mekhan-service) can share the constants without
//! depending on this crate; they are re-exported here so every existing
//! `petri_nats::Subjects` call site keeps compiling unchanged.

pub use petri_api_types::subjects::{Subjects, WorkflowContext};
