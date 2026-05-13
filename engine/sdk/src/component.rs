//! Component trait for reusable subnet patterns.
//!
//! Components are the "Integrated Circuits" of Petri net design.
//! They encapsulate common patterns (retry loops, state machines, async workers)
//! and expose only their interface points.
//!
//! # Design Philosophy
//! - **Config:** Static parameters (image, timeout, retry_limit)
//! - **Input:** Places from the outer scope wired into the component
//! - **Output:** Internal places exposed to the outer scope
//! - **Internals:** Scoped group with prefixed IDs (collision-free)
//!
//! # Example
//! ```ignore
//! pub struct AsyncWorker {
//!     name: String,
//!     image: String,
//!     retry_limit: u32,
//! }
//!
//! pub struct WorkerOutputs {
//!     pub success: PlaceHandle<JobResult>,
//!     pub failure: PlaceHandle<JobError>,
//! }
//!
//! impl Component for AsyncWorker {
//!     type Input = PlaceHandle<Job>;  // Single input
//!     type Output = WorkerOutputs;
//!
//!     fn name(&self) -> String {
//!         self.name.clone()
//!     }
//!
//!     fn instantiate(self, ctx: &mut Context, input: Self::Input) -> Self::Output {
//!         // Internal places get prefixed IDs automatically:
//!         // "worker_1/outbox", "worker_1/success", etc.
//!         let outbox = ctx.state::<Job>("outbox", "Outbox");
//!         let success = ctx.state::<JobResult>("success", "Success");
//!         let failure = ctx.state::<JobError>("failure", "Failure");
//!
//!         ctx.transition("prepare", "Prepare")
//!             .auto_input("job", &input)
//!             .auto_output("req", &outbox)
//!             .logic(r#"#{ req: job }"#);
//!
//!         // ... more transitions ...
//!
//!         WorkerOutputs { success, failure }
//!     }
//! }
//!
//! // Usage with chaining:
//! let transcode = ctx.use_component(
//!     AsyncWorker::new("Transcode", "ffmpeg:latest"),
//!     job_queue
//! );
//!
//! let notify = ctx.use_component(
//!     AsyncWorker::new("Notify", "smtp:latest"),
//!     transcode.success  // Chain components!
//! );
//! ```
//!
//! # Multiple Inputs
//! Components can accept tuples of places:
//! ```ignore
//! impl Component for CorrelatedWorker {
//!     type Input = (PlaceHandle<Request>, PlaceHandle<Signal>);
//!     type Output = PlaceHandle<Result>;
//!
//!     fn instantiate(self, ctx: &mut Context, (req, sig): Self::Input) -> Self::Output {
//!         // Use both input places...
//!     }
//! }
//!
//! let result = ctx.use_component(worker, (requests, signals));
//! ```

use crate::context::Context;

/// A reusable subnet definition (IC chip).
///
/// Components encapsulate complex patterns and expose typed Input/Output ports.
/// When instantiated via `ctx.use_component()`:
/// 1. A unique instance ID is generated (e.g., "transcode_1")
/// 2. All internal places/transitions get prefixed IDs
/// 3. A visual group is created for the component
/// 4. The component's output handles are returned
pub trait Component {
    /// Places required from the outer scope to start this component.
    ///
    /// Can be:
    /// - Single: `PlaceHandle<T>`
    /// - Tuple: `(PlaceHandle<A>, PlaceHandle<B>)`
    /// - Custom struct of handles
    type Input;

    /// Places this component exposes to the outer scope.
    ///
    /// Typically a struct containing success/failure/result handles.
    type Output;

    /// Display name for the visual group box.
    ///
    /// This name is shown in the frontend visualization.
    fn name(&self) -> String;

    /// Expand the component's internal net.
    ///
    /// Called by `ctx.use_component()` within a prefixed scope.
    /// All places and transitions created here will have prefixed IDs.
    fn instantiate(self, ctx: &mut Context, input: Self::Input) -> Self::Output;
}
