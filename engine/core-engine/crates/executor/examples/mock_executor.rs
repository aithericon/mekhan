//! Lightweight executor stub for integration testing and CI.
//!
//! Use the real `aithericon-executor-service` for demos (`just executor-demo`).
//! This stub is for environments where the full executor isn't available — it
//! subscribes to the apalis-nats job stream and for each `ExecutionJob`
//! publishes the status lifecycle (accepted → running → completed) without
//! actually executing anything.
//!
//! The job payloads arrive as apalis-nats `NatsJob<ExecutionJob>` envelopes.
//! The metadata from the inner job is echoed back in every `StatusUpdate`,
//! which is exactly what the real executor does. This lets `ExecutorWatcher`
//! extract routing info and publish signals.
//!
//! ## Usage
//!
//! ```bash
//! # Start with default settings (NATS on localhost:4333, namespace executor_jobs)
//! cargo run -p petri-executor --example mock_executor
//!
//! # Override via environment variables
//! NATS_URL=nats://my-nats:4222 EXECUTOR_NAMESPACE=my_jobs \
//!     cargo run -p petri-executor --example mock_executor
//! ```
//!
//! ## Environment Variables
//!
//! | Variable              | Default                  | Purpose                    |
//! |-----------------------|--------------------------|----------------------------|
//! | `NATS_URL`            | `nats://localhost:4333`  | NATS server address        |
//! | `EXECUTOR_NAMESPACE`  | `executor_jobs`          | Job stream namespace       |
//! | `EXECUTOR_STEP_DELAY` | `200`                    | Delay between statuses (ms)|

use std::time::Duration;

use bytes::Bytes;
use chrono::Utc;
use futures::StreamExt;

use aithericon_executor_domain::{ExecutionJob, ExecutionStatus, StatusUpdate};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".parse().unwrap()),
        )
        .init();

    let nats_url = std::env::var("NATS_URL").unwrap_or_else(|_| "nats://localhost:4333".into());
    let namespace = std::env::var("EXECUTOR_NAMESPACE").unwrap_or_else(|_| "executor_jobs".into());
    let step_delay_ms: u64 = std::env::var("EXECUTOR_STEP_DELAY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);

    tracing::info!(
        nats_url = %nats_url,
        namespace = %namespace,
        step_delay_ms = step_delay_ms,
        "Starting mock executor"
    );

    let client = async_nats::connect(&nats_url)
        .await
        .expect("connect to NATS");
    let jetstream = async_nats::jetstream::new(client);

    // Ensure the apalis-nats medium-priority job stream exists.
    // The real client also creates this idempotently, but the mock may start first.
    let stream_name = format!("{}_medium", namespace);
    let subject = format!("{}.medium", namespace);
    jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: stream_name.clone(),
            subjects: vec![subject.clone()],
            retention: async_nats::jetstream::stream::RetentionPolicy::WorkQueue,
            storage: async_nats::jetstream::stream::StorageType::File,
            max_age: Duration::from_secs(7 * 24 * 60 * 60),
            duplicate_window: Duration::from_secs(120),
            discard: async_nats::jetstream::stream::DiscardPolicy::Old,
            ..Default::default()
        })
        .await
        .expect("create job stream");

    // Ensure status stream exists (for publishing status updates).
    jetstream
        .get_or_create_stream(async_nats::jetstream::stream::Config {
            name: "EXECUTOR_STATUS".into(),
            subjects: vec!["executor.status.>".into()],
            max_age: Duration::from_secs(24 * 60 * 60),
            duplicate_window: Duration::from_secs(120),
            ..Default::default()
        })
        .await
        .expect("create status stream");

    // Create durable consumer on the medium-priority stream.
    let stream = jetstream
        .get_stream(&stream_name)
        .await
        .expect("get job stream");

    let consumer = stream
        .get_or_create_consumer(
            "mock-executor",
            async_nats::jetstream::consumer::pull::Config {
                durable_name: Some("mock-executor".into()),
                filter_subject: subject.clone(),
                ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                ..Default::default()
            },
        )
        .await
        .expect("create consumer");

    let mut messages = consumer.messages().await.expect("consumer messages");

    tracing::info!("Mock executor ready — waiting for jobs on {}", subject);

    while let Some(msg) = messages.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "message error");
                continue;
            }
        };

        // Unwrap apalis-nats NatsJob<ExecutionJob> envelope — extract the `data` field.
        let job: ExecutionJob = match serde_json::from_slice::<serde_json::Value>(&msg.payload) {
            Ok(envelope) => match envelope.get("data") {
                Some(data) => match serde_json::from_value::<ExecutionJob>(data.clone()) {
                    Ok(j) => j,
                    Err(e) => {
                        tracing::warn!(error = %e, "bad ExecutionJob in envelope, skipping");
                        let _ = msg.ack().await;
                        continue;
                    }
                },
                None => {
                    tracing::warn!("envelope missing 'data' field, skipping");
                    let _ = msg.ack().await;
                    continue;
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "bad job payload, skipping");
                let _ = msg.ack().await;
                continue;
            }
        };

        let _ = msg.ack().await;

        tracing::info!(
            execution_id = %job.execution_id,
            backend = %job.spec.backend,
            "Received job"
        );

        // Publish lifecycle: accepted → running → completed.
        for status in [
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
        ] {
            let update = StatusUpdate {
                execution_id: job.execution_id.clone(),
                // Echo the job's workspace onto the back-channel so the subject
                // carries the `{ws}` segment (falls back to the sentinel for an
                // older job envelope with an empty workspace_id).
                workspace_id: if job.workspace_id.is_empty() {
                    aithericon_executor_domain::DEFAULT_WORKSPACE.to_string()
                } else {
                    job.workspace_id.clone()
                },
                status,
                detail: serde_json::json!({}),
                metadata: job.metadata.clone(),
                source: "mock-executor".into(),
                timestamp: Utc::now(),
            };

            let payload = serde_json::to_vec(&update).expect("serialize");
            let subj = update.subject();

            let mut headers = async_nats::HeaderMap::new();
            headers.insert("Nats-Msg-Id", update.msg_id().as_str());

            match jetstream
                .publish_with_headers(subj.clone(), headers, Bytes::from(payload))
                .await
            {
                Ok(ack) => {
                    let _ = ack.await;
                    tracing::info!(
                        execution_id = %job.execution_id,
                        status = %update.status.as_str(),
                        subject = %subj,
                        "Published status"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to publish status");
                }
            }

            tokio::time::sleep(Duration::from_millis(step_delay_ms)).await;
        }
    }
}
