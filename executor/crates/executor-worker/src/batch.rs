use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use apalis_nats::NatsStorage;
use async_nats::jetstream::consumer::pull::Config as ConsumerConfig;
use futures::StreamExt;
use serde_json::json;
use tracing::{debug, info, warn};
use uuid::Uuid;

use aithericon_executor_domain::{
    BatchManifest, BatchResult, ExecutionJob, ExecutionStatus, JobResult, StatusUpdate,
};

use crate::reporter::StatusReporter;

/// Executes a batch manifest by pushing jobs through an apalis NatsStorage queue
/// and monitoring the status stream for results.
///
/// The caller is responsible for starting the apalis worker that processes jobs
/// from the same NatsStorage. This struct handles job submission and result collection.
pub struct BatchRunner {
    storage: NatsStorage<ExecutionJob>,
    reporter: StatusReporter,
    fail_fast: bool,
}

impl BatchRunner {
    pub fn new(
        storage: NatsStorage<ExecutionJob>,
        reporter: StatusReporter,
        fail_fast: bool,
    ) -> Self {
        Self {
            storage,
            reporter,
            fail_fast,
        }
    }

    /// Load a batch manifest from a JSON file.
    pub fn load_manifest(
        path: &Path,
    ) -> Result<BatchManifest, Box<dyn std::error::Error + Send + Sync>> {
        let content = std::fs::read_to_string(path)?;
        let manifest: BatchManifest = serde_json::from_str(&content)?;
        Ok(manifest)
    }

    /// Push manifest jobs through the queue and collect results from the status stream.
    ///
    /// For `fail_fast`: pushes one job at a time, waits for its terminal status,
    /// and stops on first failure.
    ///
    /// For non-fail-fast: pushes all jobs upfront, then monitors the status stream
    /// until all reach terminal status.
    pub async fn run(&self, manifest: &BatchManifest) -> BatchResult {
        if manifest.jobs.is_empty() {
            return BatchResult {
                total: 0,
                succeeded: 0,
                failed: 0,
                results: vec![],
            };
        }

        let results = if self.fail_fast {
            self.run_fail_fast(manifest).await
        } else {
            self.run_all(manifest).await
        };

        let succeeded = results
            .iter()
            .filter(|r| matches!(r.status, ExecutionStatus::Completed))
            .count();

        BatchResult {
            total: manifest.jobs.len(),
            succeeded,
            failed: results.len() - succeeded,
            results,
        }
    }

    /// Push all jobs upfront, then collect all terminal statuses.
    async fn run_all(&self, manifest: &BatchManifest) -> Vec<JobResult> {
        // Create consumer BEFORE pushing jobs to avoid missing updates
        let consumer = match self.create_batch_consumer(manifest).await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "failed to create batch status consumer");
                return vec![];
            }
        };

        // Push all jobs into the queue
        for (i, job) in manifest.jobs.iter().enumerate() {
            info!(
                execution_id = %job.execution_id,
                index = i,
                total = manifest.jobs.len(),
                "pushing batch job to queue"
            );
            if let Err(e) = self.push_job(job).await {
                warn!(
                    execution_id = %job.execution_id,
                    error = %e,
                    "failed to push job, stopping batch"
                );
                break;
            }
        }

        // Collect terminal statuses for all jobs
        let execution_ids: Vec<String> = manifest
            .jobs
            .iter()
            .map(|j| j.execution_id.clone())
            .collect();

        let terminal_map = self
            .collect_terminal_statuses(&consumer, &execution_ids, Duration::from_secs(300))
            .await;

        // Build results in manifest order
        execution_ids
            .iter()
            .filter_map(|eid| {
                terminal_map.get(eid).map(|(status, detail)| JobResult {
                    execution_id: eid.clone(),
                    status: *status,
                    duration_ms: extract_duration_ms(detail),
                    detail: detail.clone(),
                })
            })
            .collect()
    }

    /// Push one job at a time, wait for terminal status, stop on first failure.
    async fn run_fail_fast(&self, manifest: &BatchManifest) -> Vec<JobResult> {
        // Create consumer BEFORE pushing jobs
        let consumer = match self.create_batch_consumer(manifest).await {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "failed to create batch status consumer");
                return vec![];
            }
        };

        let mut results = Vec::new();

        for (i, job) in manifest.jobs.iter().enumerate() {
            info!(
                execution_id = %job.execution_id,
                index = i,
                total = manifest.jobs.len(),
                "pushing batch job to queue (fail-fast)"
            );

            if let Err(e) = self.push_job(job).await {
                warn!(
                    execution_id = %job.execution_id,
                    error = %e,
                    "failed to push job"
                );
                results.push(JobResult {
                    execution_id: job.execution_id.clone(),
                    status: ExecutionStatus::Failed,
                    duration_ms: 0,
                    detail: json!({ "error": format!("failed to push job: {e}") }),
                });
                break;
            }

            // Wait for this specific job's terminal status
            let ids = vec![job.execution_id.clone()];
            let terminal_map = self
                .collect_terminal_statuses(&consumer, &ids, Duration::from_secs(300))
                .await;

            let (status, detail) = terminal_map.get(&job.execution_id).cloned().unwrap_or((
                ExecutionStatus::Failed,
                json!({ "error": "status not received" }),
            ));

            let succeeded = matches!(status, ExecutionStatus::Completed);

            results.push(JobResult {
                execution_id: job.execution_id.clone(),
                status,
                duration_ms: extract_duration_ms(&detail),
                detail,
            });

            if !succeeded {
                info!(
                    execution_id = %job.execution_id,
                    %status,
                    "fail-fast: stopping batch after failure"
                );
                break;
            }
        }

        results
    }

    /// Push a single job into the NatsStorage queue.
    async fn push_job(
        &self,
        job: &ExecutionJob,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut storage = self.storage.clone();
        apalis_core::storage::Storage::push(&mut storage, job.clone())
            .await
            .map_err(|e| format!("push failed: {e}"))?;
        Ok(())
    }

    /// Create a JetStream pull consumer on the status stream filtered for this batch's jobs.
    async fn create_batch_consumer(
        &self,
        manifest: &BatchManifest,
    ) -> Result<
        async_nats::jetstream::consumer::PullConsumer,
        Box<dyn std::error::Error + Send + Sync>,
    > {
        let stream_name = self.reporter.status_stream_name();
        let subject_prefix = self.reporter.status_subject_prefix();

        let stream = self.reporter.jetstream().get_stream(&stream_name).await?;

        // Use a wildcard consumer that matches all status updates,
        // then filter client-side by execution_id.
        // (NATS supports only single subject filter per consumer in older versions)
        let consumer_name = format!("batch-{}", Uuid::new_v4().simple());

        let filter = format!("{subject_prefix}.>");

        debug!(
            %stream_name,
            %filter,
            %consumer_name,
            job_count = manifest.jobs.len(),
            "creating batch status consumer"
        );

        Ok(stream
            .create_consumer(ConsumerConfig {
                durable_name: Some(consumer_name),
                filter_subject: filter,
                deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::New,
                ..Default::default()
            })
            .await?)
    }

    /// Collect terminal statuses for a set of execution IDs from the status consumer.
    ///
    /// Returns when all requested IDs have reached terminal status, or timeout.
    async fn collect_terminal_statuses(
        &self,
        consumer: &async_nats::jetstream::consumer::PullConsumer,
        execution_ids: &[String],
        timeout: Duration,
    ) -> HashMap<String, (ExecutionStatus, serde_json::Value)> {
        let wanted: std::collections::HashSet<&str> =
            execution_ids.iter().map(|s| s.as_str()).collect();
        let mut results: HashMap<String, (ExecutionStatus, serde_json::Value)> = HashMap::new();
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            if results.len() >= wanted.len() {
                break;
            }

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                warn!(
                    collected = results.len(),
                    expected = wanted.len(),
                    "batch status collection timed out"
                );
                break;
            }

            let batch_result = tokio::time::timeout(
                remaining,
                consumer
                    .fetch()
                    .max_messages(10)
                    .expires(Duration::from_secs(1))
                    .messages(),
            )
            .await;

            let mut messages = match batch_result {
                Ok(Ok(msgs)) => msgs,
                Ok(Err(e)) => {
                    let err_str = e.to_string();
                    if err_str.contains("channel closed") || err_str.contains("connection closed") {
                        warn!(error = %e, "permanent consumer error, stopping");
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                Err(_) => break, // overall timeout
            };

            while let Some(msg_result) = messages.next().await {
                if let Ok(msg) = msg_result {
                    let _ = msg.ack().await;
                    if let Ok(update) = serde_json::from_slice::<StatusUpdate>(&msg.payload) {
                        if wanted.contains(update.execution_id.as_str())
                            && update.status.is_terminal()
                        {
                            debug!(
                                execution_id = %update.execution_id,
                                status = %update.status,
                                "collected terminal status"
                            );
                            results.insert(
                                update.execution_id.clone(),
                                (update.status, update.detail),
                            );
                        }
                    }
                }
            }
        }

        results
    }
}

/// Extract duration_ms from the terminal status detail JSON.
fn extract_duration_ms(detail: &serde_json::Value) -> u128 {
    detail
        .get("duration_ms")
        .and_then(|v| v.as_u64())
        .map(|v| v as u128)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manifest.json");
        std::fs::write(
            &path,
            r#"{"jobs":[{"execution_id":"j1","spec":{"backend":"process","config":{"command":"echo","args":["hi"]}},"metadata":{},"priority":"medium"}]}"#,
        )
        .unwrap();

        let manifest = BatchRunner::load_manifest(&path).unwrap();
        assert_eq!(manifest.jobs.len(), 1);
        assert_eq!(manifest.jobs[0].execution_id, "j1");
    }

    #[test]
    fn test_load_manifest_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();

        let result = BatchRunner::load_manifest(&path);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_manifest_missing_file() {
        let result = BatchRunner::load_manifest(Path::new("/nonexistent/manifest.json"));
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_duration_ms() {
        assert_eq!(extract_duration_ms(&json!({"duration_ms": 1234})), 1234);
        assert_eq!(extract_duration_ms(&json!({})), 0);
        assert_eq!(
            extract_duration_ms(&json!({"duration_ms": "not a number"})),
            0
        );
    }
}
