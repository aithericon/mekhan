//! Reusable component patterns for common Petri net structures.
//!
//! Components are the "Integrated Circuits" of Petri net design. They encapsulate
//! complex patterns and expose only their interface points.
//!
//! # Available Components
//!
//! - [`ClaimPattern`] - Lightweight resource coordination using claim handles.
//!   The adapter manages resource ownership; the workflow holds `ClaimHandle` references.
//! - [`executor_lifecycle`] - Full executor lifecycle with retry, cancel, events,
//!   effect error recovery, and dead-lettering.

mod claim;
pub mod executor_lifecycle;

pub use claim::{
    CancelledSignal, ClaimFailedJob, ClaimHandleToken, ClaimInput, ClaimJobResult, ClaimOutput,
    ClaimPattern, ClaimProcessing, ClaimRelease, CompletedSignal, ExecErrorSignal,
    InvalidationSignal,
};
pub use executor_lifecycle::{executor_lifecycle, ExecutorBridges, ExecutorLifecycleHandles};
