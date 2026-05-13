use std::sync::Arc;

use apalis_core::layers::extensions::Data;

use aithericon_executor_domain::ExecutionJob;

use crate::executor::JobExecutor;

/// Thin apalis handler that delegates to [`JobExecutor::execute`].
///
/// Returns `Ok(())` unconditionally — execution failures are application outcomes
/// reported via status updates, not infrastructure errors for apalis retry/DLQ.
pub async fn handle_execution(
    job: ExecutionJob,
    executor: Data<Arc<JobExecutor>>,
) -> Result<(), apalis_core::error::Error> {
    executor.execute(&job).await;

    // Notify the completion tracker (drain mode). No-op when tracker is None.
    if let Some(ref tracker) = executor.completion_tracker {
        tracker.record_completion();
    }

    Ok(())
}
