//! ROS execution backend for aithericon-executor.
//!
//! Interacts with a ROS (Robot Operating System) graph over a rosbridge
//! WebSocket. The endpoint is **runner-local** — configured on the executor
//! daemon via `EXECUTOR_ROS__WS_URL` (the runner advertises a reachable
//! rosbridge) rather than bound as a workspace resource.
//!
//! P1 STUB: the backend registers and reports `not yet implemented` on
//! execute. The rosbridge WebSocket client, the typedef→Port mapper, and the
//! publish/call/await operations land in P2.
//!
//! ## Crate layout
//!
//! - [`backend`] — the [`RosBackend`] `ExecutionBackend` impl.

pub mod backend;

pub use backend::RosBackend;
