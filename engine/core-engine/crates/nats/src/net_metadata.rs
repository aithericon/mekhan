//! Net metadata projection: watches lifecycle events and projects to a KV bucket.
//!
//! Consumes `NetCreated`, `NetInitialized`, `NetCompleted`, `NetCancelled` events
//! from the NATS event stream and maintains a `KV_NET_METADATA` bucket with
//! the latest metadata for each net.

use async_nats::jetstream::kv::Store;
use async_nats::jetstream::Message;
use serde::{Deserialize, Serialize};

use crate::message_loop::{run_message_loop_cancellable, MessageHandler, MessageLoopError, ProcessError};
use crate::subjects::Subjects;

/// NATS KV bucket name for net metadata.
pub const METADATA_KV_BUCKET: &str = "KV_NET_METADATA";

/// Status of a net instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetStatus {
    Created,
    Running,
    Completed,
    Cancelled,
    /// A transition failed permanently; the net was torn down. Terminal,
    /// distinct from `Completed` (success) and `Cancelled` (external request).
    Failed,
}

/// Metadata about a net instance, stored in the KV bucket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetMetadata {
    pub net_id: String,
    pub status: NetStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancelled_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
}

/// Watches the NATS event stream for lifecycle events and projects them
/// into the `KV_NET_METADATA` bucket.
pub struct NetMetadataProjection {
    jetstream: async_nats::jetstream::Context,
    kv: Store,
}

impl NetMetadataProjection {
    pub fn new(jetstream: async_nats::jetstream::Context, kv: Store) -> Self {
        Self { jetstream, kv }
    }

    /// Start the metadata projection as a spawned tokio task.
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = self.run().await {
                tracing::error!(
                    error = %e,
                    "Net metadata projection stopped with error"
                );
            }
        })
    }

    async fn run(&self) -> Result<(), MessageLoopError> {
        use async_nats::jetstream::consumer::{
            pull::Config as ConsumerConfig, AckPolicy, DeliverPolicy,
        };

        let stream = self
            .jetstream
            .get_or_create_stream(crate::stream_config())
            .await
            .map_err(|e| {
                MessageLoopError::Consumer(format!("Failed to get stream: {}", e))
            })?;

        // Subscribe to lifecycle events across all nets
        // Events are published as: petri.events.{net_id}.net.created, etc.
        let filter = format!("{}.>", Subjects::EVENTS_PREFIX);
        let consumer_name = "net-metadata-projection";

        let consumer_config = ConsumerConfig {
            durable_name: Some(consumer_name.to_string()),
            filter_subject: filter,
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::All,
            ..Default::default()
        };

        let consumer = stream
            .get_or_create_consumer(consumer_name, consumer_config)
            .await
            .map_err(|e| {
                MessageLoopError::Consumer(format!("Failed to create consumer: {}", e))
            })?;

        let handler = MetadataHandler { kv: &self.kv };

        run_message_loop_cancellable(consumer, &handler, None).await
    }

    /// Get metadata for a specific net from the KV bucket.
    pub async fn get(&self, net_id: &str) -> Result<Option<NetMetadata>, String> {
        match self.kv.get(net_id).await {
            Ok(Some(entry)) => {
                let meta: NetMetadata = serde_json::from_slice(&entry)
                    .map_err(|e| format!("Failed to parse metadata: {}", e))?;
                Ok(Some(meta))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(format!("Failed to get metadata for {}: {}", net_id, e)),
        }
    }

    /// List all net metadata entries from the KV bucket.
    pub async fn list_all(&self) -> Result<Vec<NetMetadata>, String> {
        use futures::StreamExt;

        let mut results = Vec::new();
        let keys = self
            .kv
            .keys()
            .await
            .map_err(|e| format!("Failed to list metadata keys: {}", e))?;

        tokio::pin!(keys);
        while let Some(key) = keys.next().await {
            match key {
                Ok(net_id) => {
                    if let Ok(Some(meta)) = self.get(&net_id).await {
                        results.push(meta);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Error reading metadata key");
                }
            }
        }

        Ok(results)
    }

    /// Provide access to the underlying KV store.
    pub fn kv(&self) -> &Store {
        &self.kv
    }
}

/// Handler that processes lifecycle events and updates the KV bucket.
struct MetadataHandler<'a> {
    kv: &'a Store,
}

#[async_trait::async_trait]
impl MessageHandler for MetadataHandler<'_> {
    fn listener_name(&self) -> &str {
        "net-metadata-projection"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        // Deserialize the persisted event
        let persisted: petri_domain::PersistedEvent =
            serde_json::from_slice(&msg.payload)
                .map_err(|e| ProcessError::Parse(e.to_string()))?;

        match &persisted.event {
            petri_domain::DomainEvent::NetCreated {
                net_id,
                template_id,
                parameters,
                created_by,
                label,
                ..
            } => {
                let meta = NetMetadata {
                    net_id: net_id.clone(),
                    status: NetStatus::Created,
                    template_id: template_id.clone(),
                    parameters: parameters.clone(),
                    created_at: persisted.timestamp.to_rfc3339(),
                    created_by: created_by.clone(),
                    label: label.clone(),
                    completed_at: None,
                    exit_code: None,
                    cancelled_at: None,
                    cancelled_by: None,
                    cancel_reason: None,
                };
                self.put_metadata(net_id, &meta).await?;
                tracing::debug!(net_id = %net_id, "Metadata projection: net created");
            }

            petri_domain::DomainEvent::NetInitialized { .. } => {
                // Extract net_id from the subject: petri.events.{net_id}.net.initialized
                let subject = msg.subject.as_str();
                if let Some(net_id) = extract_net_id_from_subject(subject) {
                    // Update existing entry to Running, or create a new one
                    let meta = match self.get_metadata(&net_id).await {
                        Some(mut existing) => {
                            existing.status = NetStatus::Running;
                            existing
                        }
                        None => NetMetadata {
                            net_id: net_id.clone(),
                            status: NetStatus::Running,
                            template_id: None,
                            parameters: None,
                            created_at: persisted.timestamp.to_rfc3339(),
                            created_by: None,
                            label: None,
                            completed_at: None,
                            exit_code: None,
                            cancelled_at: None,
                            cancelled_by: None,
                            cancel_reason: None,
                        },
                    };
                    self.put_metadata(&net_id, &meta).await?;
                    tracing::debug!(net_id = %net_id, "Metadata projection: net initialized → running");
                }
            }

            petri_domain::DomainEvent::NetCompleted {
                net_id,
                exit_code,
                ..
            } => {
                let meta = match self.get_metadata(net_id).await {
                    Some(mut existing) => {
                        existing.status = NetStatus::Completed;
                        existing.completed_at = Some(persisted.timestamp.to_rfc3339());
                        existing.exit_code = exit_code.clone();
                        existing
                    }
                    None => NetMetadata {
                        net_id: net_id.clone(),
                        status: NetStatus::Completed,
                        template_id: None,
                        parameters: None,
                        created_at: persisted.timestamp.to_rfc3339(),
                        created_by: None,
                        label: None,
                        completed_at: Some(persisted.timestamp.to_rfc3339()),
                        exit_code: exit_code.clone(),
                        cancelled_at: None,
                        cancelled_by: None,
                        cancel_reason: None,
                    },
                };
                self.put_metadata(net_id, &meta).await?;
                tracing::info!(net_id = %net_id, "Metadata projection: net completed");
            }

            petri_domain::DomainEvent::NetCancelled {
                net_id,
                reason,
                cancelled_by,
            } => {
                let meta = match self.get_metadata(net_id).await {
                    Some(mut existing) => {
                        existing.status = NetStatus::Cancelled;
                        existing.cancelled_at = Some(persisted.timestamp.to_rfc3339());
                        existing.cancelled_by = cancelled_by.clone();
                        existing.cancel_reason = reason.clone();
                        existing
                    }
                    None => NetMetadata {
                        net_id: net_id.clone(),
                        status: NetStatus::Cancelled,
                        template_id: None,
                        parameters: None,
                        created_at: persisted.timestamp.to_rfc3339(),
                        created_by: None,
                        label: None,
                        completed_at: None,
                        exit_code: None,
                        cancelled_at: Some(persisted.timestamp.to_rfc3339()),
                        cancelled_by: cancelled_by.clone(),
                        cancel_reason: reason.clone(),
                    },
                };
                self.put_metadata(net_id, &meta).await?;
                tracing::info!(net_id = %net_id, "Metadata projection: net cancelled");
            }

            petri_domain::DomainEvent::NetFailed {
                net_id,
                transition_id,
                reason,
                retryable,
            } => {
                // Reuse the cancellation fields as the generic "terminal stop"
                // record (no NetMetadata schema change): `cancel_reason` holds
                // the failure detail, `cancelled_by` notes it was the engine.
                let detail = format!(
                    "transition {} failed permanently (retryable={}): {}",
                    transition_id, retryable, reason
                );
                let meta = match self.get_metadata(net_id).await {
                    Some(mut existing) => {
                        existing.status = NetStatus::Failed;
                        existing.cancelled_at = Some(persisted.timestamp.to_rfc3339());
                        existing.cancelled_by = Some("engine".to_string());
                        existing.cancel_reason = Some(detail);
                        existing
                    }
                    None => NetMetadata {
                        net_id: net_id.clone(),
                        status: NetStatus::Failed,
                        template_id: None,
                        parameters: None,
                        created_at: persisted.timestamp.to_rfc3339(),
                        created_by: None,
                        label: None,
                        completed_at: None,
                        exit_code: None,
                        cancelled_at: Some(persisted.timestamp.to_rfc3339()),
                        cancelled_by: Some("engine".to_string()),
                        cancel_reason: Some(detail),
                    },
                };
                self.put_metadata(net_id, &meta).await?;
                tracing::warn!(net_id = %net_id, "Metadata projection: net failed");
            }

            // Ignore all other events
            _ => {}
        }

        Ok(())
    }
}

impl MetadataHandler<'_> {
    async fn put_metadata(&self, net_id: &str, meta: &NetMetadata) -> Result<(), ProcessError> {
        let value =
            serde_json::to_vec(meta).map_err(|e| ProcessError::Business(e.to_string()))?;
        self.kv
            .put(net_id, value.into())
            .await
            .map_err(|e| {
                ProcessError::Business(format!("Failed to put metadata for {}: {}", net_id, e))
            })?;
        Ok(())
    }

    async fn get_metadata(&self, net_id: &str) -> Option<NetMetadata> {
        match self.kv.get(net_id).await {
            Ok(Some(entry)) => serde_json::from_slice(&entry).ok(),
            _ => None,
        }
    }
}

/// Extract net_id from a NATS event subject like `petri.events.{net_id}.net.initialized`.
fn extract_net_id_from_subject(subject: &str) -> Option<String> {
    let parts: Vec<&str> = subject.split('.').collect();
    // petri.events.{net_id}.{event_type...}
    if parts.len() >= 4 && parts[0] == "petri" && parts[1] == "events" {
        Some(parts[2].to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_net_metadata_roundtrip() {
        let meta = NetMetadata {
            net_id: "test-net".to_string(),
            status: NetStatus::Running,
            template_id: Some("template-1".to_string()),
            parameters: Some(serde_json::json!({"key": "value"})),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            created_by: Some("admin".to_string()),
            label: Some("Test Net".to_string()),
            completed_at: None,
            exit_code: None,
            cancelled_at: None,
            cancelled_by: None,
            cancel_reason: None,
        };

        let json = serde_json::to_string(&meta).unwrap();
        let parsed: NetMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.net_id, "test-net");
        assert_eq!(parsed.status, NetStatus::Running);
        assert_eq!(parsed.template_id, Some("template-1".to_string()));
    }

    #[test]
    fn test_net_status_serialization() {
        assert_eq!(
            serde_json::to_string(&NetStatus::Created).unwrap(),
            "\"created\""
        );
        assert_eq!(
            serde_json::to_string(&NetStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&NetStatus::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&NetStatus::Cancelled).unwrap(),
            "\"cancelled\""
        );
    }

    #[test]
    fn test_extract_net_id_from_subject() {
        assert_eq!(
            extract_net_id_from_subject("petri.events.my-net.net.initialized"),
            Some("my-net".to_string())
        );
        assert_eq!(
            extract_net_id_from_subject("petri.events.net-123.token.created"),
            Some("net-123".to_string())
        );
        assert_eq!(extract_net_id_from_subject("petri.events"), None);
        assert_eq!(extract_net_id_from_subject("invalid.subject"), None);
    }
}
