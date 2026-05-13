//! NATS-based human task client.
//!
//! Submits human task requests to the `human.request.{net_id}.{place}` subject.
//! Publishes cancel requests to the `human.cancel.{net_id}.{place}` subject.
//! Uses dedicated HUMAN_REQUESTS and HUMAN_CANCEL streams separate from PETRI_GLOBAL.

use async_nats::jetstream;
use async_nats::jetstream::stream::{Config as StreamConfig, RetentionPolicy};
use petri_domain::human::{HumanTaskCancellation, HumanTaskClient, HumanTaskRequest};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use crate::subjects::Subjects;

/// Stream name for human task requests.
pub const HUMAN_STREAM_NAME: &str = "HUMAN_REQUESTS";

/// Stream name for human task cancel requests.
pub const HUMAN_CANCEL_STREAM_NAME: &str = "HUMAN_CANCEL";

/// NATS client for human tasks.
#[derive(Debug)]
pub struct HumanNatsClient {
    jetstream: jetstream::Context,
    net_id: String,
    org_id: Option<String>,
    request_stream_ensured: AtomicBool,
    cancel_stream_ensured: AtomicBool,
}

impl HumanNatsClient {
    /// Create a new human NATS client.
    pub fn new(jetstream: jetstream::Context, net_id: String, org_id: Option<String>) -> Self {
        Self {
            jetstream,
            net_id,
            org_id,
            request_stream_ensured: AtomicBool::new(false),
            cancel_stream_ensured: AtomicBool::new(false),
        }
    }

    /// Ensure the HUMAN_REQUESTS stream exists.
    async fn ensure_request_stream(&self) -> Result<(), String> {
        if self.request_stream_ensured.load(Ordering::SeqCst) {
            return Ok(());
        }

        let config = StreamConfig {
            name: HUMAN_STREAM_NAME.to_string(),
            subjects: vec![format!("{}.>", Subjects::HUMAN_REQUEST_PREFIX)],
            retention: RetentionPolicy::Limits,
            max_age: Duration::from_secs(7 * 24 * 60 * 60), // 7 days
            ..Default::default()
        };

        self.jetstream
            .get_or_create_stream(config)
            .await
            .map_err(|e| format!("Failed to ensure HUMAN_REQUESTS stream: {}", e))?;

        self.request_stream_ensured.store(true, Ordering::SeqCst);
        tracing::info!("HUMAN_REQUESTS stream ensured");
        Ok(())
    }

    /// Ensure the HUMAN_CANCEL stream exists.
    async fn ensure_cancel_stream(&self) -> Result<(), String> {
        if self.cancel_stream_ensured.load(Ordering::SeqCst) {
            return Ok(());
        }

        let config = StreamConfig {
            name: HUMAN_CANCEL_STREAM_NAME.to_string(),
            subjects: vec![format!("{}.>", Subjects::HUMAN_CANCEL_PREFIX)],
            retention: RetentionPolicy::Limits,
            max_age: Duration::from_secs(7 * 24 * 60 * 60), // 7 days
            ..Default::default()
        };

        self.jetstream
            .get_or_create_stream(config)
            .await
            .map_err(|e| format!("Failed to ensure HUMAN_CANCEL stream: {}", e))?;

        self.cancel_stream_ensured.store(true, Ordering::SeqCst);
        tracing::info!("HUMAN_CANCEL stream ensured");
        Ok(())
    }
}

#[async_trait::async_trait]
impl HumanTaskClient for HumanNatsClient {
    async fn submit_task(&self, mut request: HumanTaskRequest) -> Result<String, String> {
        // Ensure stream exists before publishing
        self.ensure_request_stream().await?;

        // Ensure net_id is set
        if request.net_id.is_none() {
            request.net_id = Some(self.net_id.clone());
        }

        let net_id = request.net_id.clone().unwrap();
        let place = request.place.clone().unwrap_or_else(|| "default".to_string());
        let task_id = request.task_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let subject = Subjects::human_request(&net_id, &place);

        let payload = serde_json::to_vec(&request)
            .map_err(|e| format!("Failed to serialize human task request: {}", e))?;

        // Publish to NATS JetStream for reliability
        match self.jetstream.publish(subject, payload.into()).await {
            Ok(ack_future) => {
                match ack_future.await {
                    Ok(_) => Ok(task_id),
                    Err(e) => Err(format!("NATS publish acknowledgment failed: {}", e)),
                }
            }
            Err(e) => Err(format!("NATS publish failed: {}", e)),
        }
    }

    async fn cancel_task(
        &self,
        task_id: &str,
        place: &str,
        reason: Option<&str>,
    ) -> Result<(), String> {
        self.ensure_cancel_stream().await?;

        let cancellation = HumanTaskCancellation {
            task_id: task_id.to_string(),
            reason: reason.map(|r| r.to_string()),
            cancelled_at: chrono::Utc::now(),
        };

        let subject = Subjects::human_cancel(&self.net_id, place);

        let payload = serde_json::to_vec(&cancellation)
            .map_err(|e| format!("Failed to serialize cancel request: {}", e))?;

        match self.jetstream.publish(subject, payload.into()).await {
            Ok(ack_future) => {
                match ack_future.await {
                    Ok(_) => {
                        tracing::info!(
                            task_id = %task_id,
                            place = %place,
                            "Human task cancel request published"
                        );
                        Ok(())
                    }
                    Err(e) => Err(format!("NATS cancel publish ack failed: {}", e)),
                }
            }
            Err(e) => Err(format!("NATS cancel publish failed: {}", e)),
        }
    }

    fn name(&self) -> &str {
        "human-nats"
    }

    fn net_id(&self) -> &str {
        &self.net_id
    }

    fn org_id(&self) -> Option<&str> {
        self.org_id.as_deref()
    }
}
