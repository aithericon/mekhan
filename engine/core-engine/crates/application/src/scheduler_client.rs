//! Mock scheduler client for testing effect handlers without a real scheduler.
//!
//! `MockSchedulerClient` simulates job submission and cancellation with
//! configurable behavior for testing different scenarios.

use std::collections::HashMap;
use std::sync::Mutex;

use petri_domain::{JobStatus, SchedulerClient, SchedulerError, SubmitRequest, SubmitResult};

/// Mock scheduler client for testing.
///
/// Tracks submitted and cancelled jobs. By default, submissions succeed
/// immediately with a generated scheduler job ID.
pub struct MockSchedulerClient {
    name: String,
    /// Counter for generating unique scheduler job IDs.
    counter: Mutex<u64>,
    /// Submitted jobs: signal_key → scheduler_job_id
    submitted: Mutex<HashMap<String, String>>,
    /// Cancelled scheduler job IDs.
    cancelled: Mutex<Vec<String>>,
    /// If true, all submissions fail.
    fail_submissions: bool,
}

impl MockSchedulerClient {
    /// Create a mock client that succeeds on all operations.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            counter: Mutex::new(0),
            submitted: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(Vec::new()),
            fail_submissions: false,
        }
    }

    /// Create a mock client that fails all submissions.
    pub fn always_fail(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            counter: Mutex::new(0),
            submitted: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(Vec::new()),
            fail_submissions: true,
        }
    }

    /// Get the list of submitted correlation keys.
    pub fn submitted_keys(&self) -> Vec<String> {
        self.submitted.lock().unwrap().keys().cloned().collect()
    }

    /// Get the list of cancelled scheduler job IDs.
    pub fn cancelled_ids(&self) -> Vec<String> {
        self.cancelled.lock().unwrap().clone()
    }

    /// Get the scheduler job ID for a correlation key.
    pub fn get_scheduler_job_id(&self, signal_key: &str) -> Option<String> {
        self.submitted.lock().unwrap().get(signal_key).cloned()
    }
}

#[async_trait::async_trait]
impl SchedulerClient for MockSchedulerClient {
    async fn submit(&self, request: SubmitRequest) -> Result<SubmitResult, SchedulerError> {
        if self.fail_submissions {
            return Err(SchedulerError::SubmissionFailed(
                "Mock client configured to fail".into(),
            ));
        }

        let mut counter = self.counter.lock().unwrap();
        *counter += 1;
        let scheduler_job_id = format!("mock-{}-{}", self.name, counter);

        self.submitted
            .lock()
            .unwrap()
            .insert(request.signal_key, scheduler_job_id.clone());

        Ok(SubmitResult { scheduler_job_id })
    }

    async fn cancel(&self, scheduler_job_id: &str) -> Result<(), SchedulerError> {
        self.cancelled
            .lock()
            .unwrap()
            .push(scheduler_job_id.to_string());
        Ok(())
    }

    async fn status(&self, scheduler_job_id: &str) -> Result<JobStatus, SchedulerError> {
        // Check if cancelled
        if self
            .cancelled
            .lock()
            .unwrap()
            .contains(&scheduler_job_id.to_string())
        {
            return Ok(JobStatus::Cancelled);
        }
        // Check if submitted (still running)
        let submitted = self.submitted.lock().unwrap();
        if submitted.values().any(|id| id == scheduler_job_id) {
            return Ok(JobStatus::Running);
        }
        Err(SchedulerError::QueryFailed(format!(
            "Unknown job: {}",
            scheduler_job_id
        )))
    }

    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_mock_submit_success() {
        let client = MockSchedulerClient::new("test");

        let result = client
            .submit(SubmitRequest {
                job_template_id: "template-1".into(),
                signal_key: "job-1:0".into(),
                execution_id: "exec-job-1-0".into(),
                token_data: json!({"model": "resnet"}),
            })
            .await
            .unwrap();

        assert!(result.scheduler_job_id.starts_with("mock-test-"));
        assert_eq!(client.submitted_keys(), vec!["job-1:0"]);
    }

    #[tokio::test]
    async fn test_mock_submit_increments_counter() {
        let client = MockSchedulerClient::new("test");

        let r1 = client
            .submit(SubmitRequest {
                job_template_id: "t".into(),
                signal_key: "a:0".into(),
                execution_id: "exec-a-0".into(),
                token_data: json!({}),
            })
            .await
            .unwrap();

        let r2 = client
            .submit(SubmitRequest {
                job_template_id: "t".into(),
                signal_key: "b:0".into(),
                execution_id: "exec-b-0".into(),
                token_data: json!({}),
            })
            .await
            .unwrap();

        assert_ne!(r1.scheduler_job_id, r2.scheduler_job_id);
    }

    #[tokio::test]
    async fn test_mock_always_fail() {
        let client = MockSchedulerClient::always_fail("test");

        let result = client
            .submit(SubmitRequest {
                job_template_id: "t".into(),
                signal_key: "a:0".into(),
                execution_id: "exec-fail".into(),
                token_data: json!({}),
            })
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mock_cancel() {
        let client = MockSchedulerClient::new("test");

        client.cancel("mock-test-1").await.unwrap();

        assert_eq!(client.cancelled_ids(), vec!["mock-test-1"]);
    }

    #[tokio::test]
    async fn test_mock_status_running() {
        let client = MockSchedulerClient::new("test");

        let result = client
            .submit(SubmitRequest {
                job_template_id: "t".into(),
                signal_key: "a:0".into(),
                execution_id: "exec-running".into(),
                token_data: json!({}),
            })
            .await
            .unwrap();

        let status = client.status(&result.scheduler_job_id).await.unwrap();
        assert_eq!(status, JobStatus::Running);
    }

    #[tokio::test]
    async fn test_mock_status_cancelled() {
        let client = MockSchedulerClient::new("test");

        let result = client
            .submit(SubmitRequest {
                job_template_id: "t".into(),
                signal_key: "a:0".into(),
                execution_id: "exec-cancelled".into(),
                token_data: json!({}),
            })
            .await
            .unwrap();

        client.cancel(&result.scheduler_job_id).await.unwrap();

        let status = client.status(&result.scheduler_job_id).await.unwrap();
        assert_eq!(status, JobStatus::Cancelled);
    }

    #[tokio::test]
    async fn test_mock_status_unknown() {
        let client = MockSchedulerClient::new("test");

        let result = client.status("nonexistent").await;
        assert!(result.is_err());
    }
}
