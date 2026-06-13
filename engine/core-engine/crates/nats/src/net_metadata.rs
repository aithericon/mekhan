//! Net metadata projection: watches lifecycle events and projects to a KV bucket.
//!
//! Consumes `NetCreated`, `NetInitialized`, `NetCompleted`, `NetCancelled` events
//! from the NATS event stream and maintains a `KV_NET_METADATA` bucket with
//! the latest metadata for each net.

use async_nats::jetstream::kv::Store;
use async_nats::jetstream::Message;
use serde::{Deserialize, Serialize};

use crate::message_loop::{
    run_message_loop_cancellable, MessageHandler, MessageLoopError, ProcessError,
};
use crate::subjects::Subjects;

/// Base NATS KV bucket name for net metadata.
///
/// The live bucket is per-workspace: `KV_NET_METADATA_{ws}`, built with
/// [`crate::kv_bucket_for`]. The caller opens the workspace-scoped bucket and
/// passes the [`Store`] into [`NetMetadataProjection::new`] alongside the
/// matching `workspace_id`.
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
    /// Multi-tenancy: the workspace (tenant) this net belongs to, derived from
    /// the event subject (`petri.{ws}.{net}.events.*`) by the projection. This
    /// is a KV-internal field (NOT part of the hash-chained `DomainEvent`), so
    /// adding it is safe. It lets the net_id-keyed global metadata index double
    /// as a net_id → workspace resolver — the woken-net wake path reads it to
    /// stamp the correct tenant before hydrating (hazard #2). Defaults to
    /// `DEFAULT_WORKSPACE` for legacy entries written before this field existed.
    #[serde(default = "default_workspace")]
    pub workspace_id: String,
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

fn default_workspace() -> String {
    Subjects::DEFAULT_WORKSPACE.to_string()
}

/// Watches the NATS event stream for lifecycle events and projects them into
/// per-tenant metadata KV.
///
/// This is a SINGLE GLOBAL consumer over every workspace's lifecycle events
/// (`petri.*.events.>`). For each event it derives the workspace from the
/// subject (`petri.{ws}.{net}.events.*`) and DUAL-WRITES:
///
/// 1. the global net_id-keyed index bucket [`METADATA_KV_BUCKET`]
///    (`KV_NET_METADATA`) — the index every net_id-only reader uses
///    (tombstone gates, discovery endpoint, woken-net workspace resolver). The
///    entry now carries `workspace_id`, so the index doubles as a
///    net_id → workspace map.
/// 2. the per-tenant bucket `KV_NET_METADATA_{ws}` (via [`crate::kv_bucket_for`])
///    — so two tenants hosted in one engine process never share metadata state.
///    These per-ws stores are opened lazily and cached.
///
/// TODO(stream-per-ws): once the stream is sharded per workspace, this becomes
/// one `net-metadata-projection-{ws}` durable per tenant filtering
/// `petri.{ws}.*.events.>`, and the global index bucket can be dropped in favor
/// of a workspace-scoped discovery API.
pub struct NetMetadataProjection {
    jetstream: async_nats::jetstream::Context,
    /// Global net_id-keyed index bucket (`KV_NET_METADATA`). Every net_id-only
    /// reader (tombstone gate, discovery, woken-ws resolver) reads this.
    kv: Store,
}

impl NetMetadataProjection {
    /// Create the global metadata projection.
    ///
    /// `kv` is the global net_id-keyed index bucket ([`METADATA_KV_BUCKET`]);
    /// per-tenant `KV_NET_METADATA_{ws}` buckets are opened lazily from
    /// `jetstream` as workspaces are observed on the event stream.
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
            .map_err(|e| MessageLoopError::Consumer(format!("Failed to get stream: {}", e)))?;

        // Subscribe to lifecycle events across ALL nets in ALL workspaces.
        // Events are published as `petri.{ws}.{net_id}.events.{suffix}`; the two
        // leading wildcards span ws and net: `petri.*.*.events.>`. The ws is
        // recovered per-event from the subject so the projection can write the
        // per-tenant bucket.
        // TODO(stream-per-ws): split into per-workspace durables filtered on
        // `petri.{ws}.*.events.>`.
        let filter = Subjects::net_events_filter("*", "*");
        let consumer_name = "net-metadata-projection".to_string();

        let consumer_config = ConsumerConfig {
            durable_name: Some(consumer_name.clone()),
            filter_subject: filter,
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::All,
            ..Default::default()
        };

        let consumer = stream
            .get_or_create_consumer(&consumer_name, consumer_config)
            .await
            .map_err(|e| MessageLoopError::Consumer(format!("Failed to create consumer: {}", e)))?;

        let handler = MetadataHandler {
            jetstream: self.jetstream.clone(),
            index_kv: self.kv.clone(),
            per_ws_kv: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        };

        run_message_loop_cancellable(consumer, &handler, None, None).await
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

/// Handler that processes lifecycle events and updates the per-tenant KV.
///
/// Owns the global net_id-keyed index bucket plus a lazily-populated cache of
/// per-workspace `KV_NET_METADATA_{ws}` stores, both written on every event
/// (dual-write) so net_id-only readers and per-tenant isolation both hold.
struct MetadataHandler {
    jetstream: async_nats::jetstream::Context,
    index_kv: Store,
    per_ws_kv: tokio::sync::Mutex<std::collections::HashMap<String, Store>>,
}

#[async_trait::async_trait]
impl MessageHandler for MetadataHandler {
    fn listener_name(&self) -> &str {
        "net-metadata-projection"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        // Deserialize the persisted event
        let persisted: petri_domain::PersistedEvent =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        // Recover the workspace from the delivered subject
        // (`petri.{ws}.{net}.events.*`). The single global consumer spans all
        // tenants, so the concrete workspace lives only on the subject — it is
        // stamped onto every metadata entry and selects the per-tenant bucket.
        let ws = extract_workspace_from_subject(msg.subject.as_str())
            .unwrap_or_else(|| Subjects::DEFAULT_WORKSPACE.to_string());

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
                    workspace_id: ws.clone(),
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
                self.put_metadata(net_id, &ws, &meta).await?;
                tracing::debug!(net_id = %net_id, "Metadata projection: net created");
            }

            petri_domain::DomainEvent::NetInitialized { .. } => {
                // Extract net_id from the subject:
                // petri.{ws}.{net_id}.events.{suffix}
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
                            workspace_id: ws.clone(),
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
                    self.put_metadata(&net_id, &ws, &meta).await?;
                    tracing::debug!(net_id = %net_id, "Metadata projection: net initialized → running");
                }
            }

            petri_domain::DomainEvent::NetCompleted {
                net_id, exit_code, ..
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
                        workspace_id: ws.clone(),
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
                self.put_metadata(net_id, &ws, &meta).await?;
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
                        workspace_id: ws.clone(),
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
                self.put_metadata(net_id, &ws, &meta).await?;
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
                        workspace_id: ws.clone(),
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
                self.put_metadata(net_id, &ws, &meta).await?;
                tracing::warn!(net_id = %net_id, "Metadata projection: net failed");
            }

            // Ignore all other events
            _ => {}
        }

        Ok(())
    }
}

impl MetadataHandler {
    /// Open (and cache) the per-workspace `KV_NET_METADATA_{ws}` store.
    async fn per_ws_store(&self, ws: &str) -> Option<Store> {
        {
            let cache = self.per_ws_kv.lock().await;
            if let Some(store) = cache.get(ws) {
                return Some(store.clone());
            }
        }
        let bucket = crate::kv_bucket_for(METADATA_KV_BUCKET, ws);
        // get-or-create: the projection may observe a workspace before any
        // other component opened its bucket.
        let store = match self.jetstream.get_key_value(&bucket).await {
            Ok(s) => Some(s),
            Err(_) => match self
                .jetstream
                .create_key_value(async_nats::jetstream::kv::Config {
                    bucket: bucket.clone(),
                    history: 1,
                    ..Default::default()
                })
                .await
            {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!(bucket = %bucket, error = %e,
                        "Failed to open per-workspace metadata bucket");
                    None
                }
            },
        };
        if let Some(ref s) = store {
            self.per_ws_kv
                .lock()
                .await
                .insert(ws.to_string(), s.clone());
        }
        store
    }

    /// DUAL-WRITE the metadata: stamp `workspace_id` (the event subject is
    /// authoritative), then write the global net_id-keyed index AND the
    /// per-tenant `KV_NET_METADATA_{ws}` bucket.
    async fn put_metadata(
        &self,
        net_id: &str,
        ws: &str,
        meta: &NetMetadata,
    ) -> Result<(), ProcessError> {
        let mut meta = meta.clone();
        meta.workspace_id = ws.to_string();
        let value = serde_json::to_vec(&meta).map_err(|e| ProcessError::Business(e.to_string()))?;

        // 1. Global index bucket (net_id-keyed; readers that lack a ws use this).
        self.index_kv
            .put(net_id, value.clone().into())
            .await
            .map_err(|e| {
                ProcessError::Business(format!("Failed to put index metadata for {}: {}", net_id, e))
            })?;

        // 2. Per-tenant bucket (isolation). Best-effort: if the bucket can't be
        // opened the index write above still keeps the net discoverable.
        // TODO(stream-per-ws): once metadata is sharded per workspace, the
        // per-ws bucket becomes the source of truth and the index is dropped.
        if let Some(store) = self.per_ws_store(ws).await {
            if let Err(e) = store.put(net_id, value.into()).await {
                tracing::warn!(net_id = %net_id, workspace = %ws, error = %e,
                    "Failed to put per-workspace metadata (index write succeeded)");
            }
        }
        Ok(())
    }

    async fn get_metadata(&self, net_id: &str) -> Option<NetMetadata> {
        match self.index_kv.get(net_id).await {
            Ok(Some(entry)) => serde_json::from_slice(&entry).ok(),
            _ => None,
        }
    }
}

/// Extract the workspace (tenant) from a ws-segmented event subject like
/// `petri.{ws}.{net_id}.events.{suffix}` (ADR-09 layout).
fn extract_workspace_from_subject(subject: &str) -> Option<String> {
    let parts: Vec<&str> = subject.split('.').collect();
    if parts.len() >= 5
        && parts[0] == Subjects::PETRI_ROOT
        && parts[3] == Subjects::EVENTS_CATEGORY
    {
        Some(parts[1].to_string())
    } else {
        None
    }
}

/// Extract net_id from a ws-segmented NATS event subject like
/// `petri.{ws}.{net_id}.events.{suffix}` (ADR-09 layout).
fn extract_net_id_from_subject(subject: &str) -> Option<String> {
    let parts: Vec<&str> = subject.split('.').collect();
    // petri.{ws}.{net_id}.events.{event_type...}
    if parts.len() >= 5
        && parts[0] == Subjects::PETRI_ROOT
        && parts[3] == Subjects::EVENTS_CATEGORY
    {
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
            workspace_id: "ws1".to_string(),
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
        assert_eq!(parsed.workspace_id, "ws1");
        assert_eq!(parsed.template_id, Some("template-1".to_string()));
    }

    #[test]
    fn test_workspace_id_defaults_for_legacy_entry() {
        // A metadata blob written before `workspace_id` existed must still
        // deserialize, defaulting to DEFAULT_WORKSPACE.
        let legacy = serde_json::json!({
            "net_id": "old-net",
            "status": "running",
            "created_at": "2024-01-01T00:00:00Z"
        });
        let parsed: NetMetadata = serde_json::from_value(legacy).unwrap();
        assert_eq!(parsed.workspace_id, Subjects::DEFAULT_WORKSPACE);
    }

    #[test]
    fn test_extract_workspace_from_subject() {
        assert_eq!(
            extract_workspace_from_subject("petri.default.my-net.events.net.initialized"),
            Some("default".to_string())
        );
        assert_eq!(
            extract_workspace_from_subject("petri.ws1.net-123.events.token.created"),
            Some("ws1".to_string())
        );
        // Old (pre-ws) scheme no longer matches.
        assert_eq!(
            extract_workspace_from_subject("petri.events.my-net.net.initialized"),
            None
        );
        assert_eq!(extract_workspace_from_subject("petri.events"), None);
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
            extract_net_id_from_subject("petri.default.my-net.events.net.initialized"),
            Some("my-net".to_string())
        );
        assert_eq!(
            extract_net_id_from_subject("petri.ws1.net-123.events.token.created"),
            Some("net-123".to_string())
        );
        // Old (pre-ws) scheme no longer matches.
        assert_eq!(
            extract_net_id_from_subject("petri.events.my-net.net.initialized"),
            None
        );
        assert_eq!(extract_net_id_from_subject("petri.events"), None);
        assert_eq!(extract_net_id_from_subject("invalid.subject"), None);
    }
}
