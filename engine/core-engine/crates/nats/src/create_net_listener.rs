//! NATS command listener for creating new net instances.
//!
//! Subscribes to `petri.commands.create_net` and creates net instances
//! on demand, enabling programmatic/automated net provisioning.

use async_nats::jetstream::Message;
use serde::{Deserialize, Serialize};

use crate::message_loop::{
    run_message_loop_cancellable, MessageHandler, MessageLoopError, ProcessError,
};
use crate::subjects::Subjects;

/// A token to inject into a specific place after scenario loading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitialToken {
    /// Target place identifier.
    pub place_id: String,
    /// Token data (JSON value).
    pub token: serde_json::Value,
    /// Optional reply routing context for request-reply bridge patterns (e.g., from spawn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_routing: Option<petri_domain::ReplyRouting>,
}

/// Request payload for creating a new net via NATS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateNetRequest {
    /// Net identifier (auto-generated UUID if not provided).
    pub net_id: String,
    /// Scenario definition in AIR JSON format.
    pub scenario: serde_json::Value,
    /// Optional template identifier for tracking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    /// Optional parameters for the net.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<serde_json::Value>,
    /// Who created this net (for audit trail).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// Human-readable label for display in the UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Optional tokens to inject into specific places after scenario loading.
    /// Used by SpawnNetHandler to atomically deliver the initial token with net creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_tokens: Option<Vec<InitialToken>>,
}

/// Response payload for the create-net command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateNetResponse {
    pub success: bool,
    pub net_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Trait for decoupling net creation logic from the NetRegistry.
#[async_trait::async_trait]
pub trait NetCreator: Send + Sync {
    /// Create a new net instance and load the scenario under `workspace`.
    ///
    /// Multi-tenancy: `workspace` is recovered from the create-net subject
    /// (`petri.{ws}.commands.create_net`) by the listener — it is NOT a field on
    /// `CreateNetRequest` (keeps the wire contract stable and matches the
    /// wildcard-ws subject filter). The creator stamps it onto the spawned net's
    /// service BEFORE `initialize()` so the child publishes under the parent's
    /// tenant rather than `DEFAULT_WORKSPACE` (hazard #3 — cross-tenant leak for
    /// sub-workflows).
    async fn create_and_load(
        &self,
        request: &CreateNetRequest,
        workspace: &str,
    ) -> Result<(), String>;
}

/// Listens for `petri.commands.create_net` messages and creates net instances.
pub struct CreateNetListener {
    jetstream: async_nats::jetstream::Context,
    creator: std::sync::Arc<dyn NetCreator>,
    consumer_name: String,
}

impl CreateNetListener {
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        creator: std::sync::Arc<dyn NetCreator>,
    ) -> Self {
        Self {
            jetstream,
            creator,
            consumer_name: "create-net-listener".to_string(),
        }
    }

    /// Set a custom consumer name (useful for testing with isolated consumers).
    pub fn with_consumer_name(mut self, name: impl Into<String>) -> Self {
        self.consumer_name = name.into();
        self
    }

    /// Start the listener as a spawned tokio task.
    pub fn start(self: std::sync::Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = self.run().await {
                tracing::error!(
                    error = %e,
                    "Create-net listener stopped with error"
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

        // Single global listener spanning ALL workspaces. The create_net
        // subject is ws-segmented (`petri.{ws}.commands.create_net`), so we
        // filter with a `*` wildcard over the workspace token and recover the
        // concrete workspace from the delivered subject in the handler (threaded
        // into `create_and_load` so the child net is stamped under the parent's
        // tenant — hazard #3).
        // TODO(stream-per-ws): split into per-workspace durables.
        let filter_subject = format!(
            "{}.*.{}.{}",
            Subjects::PETRI_ROOT,
            Subjects::COMMANDS_CATEGORY,
            Subjects::COMMAND_CREATE_NET_SUFFIX
        );

        let consumer_config = ConsumerConfig {
            durable_name: Some(self.consumer_name.clone()),
            filter_subject,
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::New,
            ..Default::default()
        };

        let consumer = stream
            .get_or_create_consumer(&self.consumer_name, consumer_config)
            .await
            .map_err(|e| MessageLoopError::Consumer(format!("Failed to create consumer: {}", e)))?;

        let handler = CreateNetHandler {
            creator: &self.creator,
        };

        run_message_loop_cancellable(
            consumer,
            &handler,
            None,
            Some(crate::dlq::DlqPublisher::new(self.jetstream.clone())),
        )
        .await
    }
}

/// Handler for the create-net command listener.
struct CreateNetHandler<'a> {
    creator: &'a std::sync::Arc<dyn NetCreator>,
}

#[async_trait::async_trait]
impl MessageHandler for CreateNetHandler<'_> {
    fn listener_name(&self) -> &str {
        "create-net"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let request: CreateNetRequest = serde_json::from_slice(&msg.payload)
            .map_err(|e| ProcessError::Parse(format!("Invalid CreateNetRequest: {}", e)))?;

        // Recover the workspace from the delivered subject
        // (`petri.{ws}.commands.create_net`). The listener filters wildcard-ws,
        // so the concrete tenant lives only on the subject — thread it into
        // `create_and_load` so the spawned child net is stamped under the
        // parent's workspace (hazard #3). Falls back to DEFAULT_WORKSPACE for a
        // malformed/legacy subject.
        let workspace = Subjects::parse_create_net_subject(&msg.subject)
            .unwrap_or(Subjects::DEFAULT_WORKSPACE)
            .to_string();

        tracing::info!(
            net_id = %request.net_id,
            workspace = %workspace,
            template_id = ?request.template_id,
            created_by = ?request.created_by,
            "Create-net command received"
        );

        self.creator
            .create_and_load(&request, &workspace)
            .await
            .map_err(ProcessError::Business)?;

        tracing::info!(
            net_id = %request.net_id,
            "Net created and scenario loaded via NATS command"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_net_request_roundtrip() {
        let request = CreateNetRequest {
            net_id: "test-net".to_string(),
            scenario: serde_json::json!({
                "places": [],
                "transitions": []
            }),
            template_id: Some("template-1".to_string()),
            parameters: Some(serde_json::json!({"gpu_count": 4})),
            created_by: Some("admin".to_string()),
            label: Some("Test Net".to_string()),
            initial_tokens: None,
        };

        let json = serde_json::to_string(&request).unwrap();
        let parsed: CreateNetRequest = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.net_id, "test-net");
        assert_eq!(parsed.template_id, Some("template-1".to_string()));
        assert_eq!(parsed.created_by, Some("admin".to_string()));
    }

    #[test]
    fn test_create_net_request_minimal() {
        let json = serde_json::json!({
            "net_id": "my-net",
            "scenario": {"places": [], "transitions": []}
        });

        let request: CreateNetRequest = serde_json::from_value(json).unwrap();
        assert_eq!(request.net_id, "my-net");
        assert!(request.template_id.is_none());
        assert!(request.parameters.is_none());
        assert!(request.created_by.is_none());
        assert!(request.initial_tokens.is_none());
    }

    #[test]
    fn test_create_net_request_with_initial_tokens() {
        let request = CreateNetRequest {
            net_id: "spawn-child".to_string(),
            scenario: serde_json::json!({"places": [], "transitions": []}),
            template_id: None,
            parameters: Some(serde_json::json!({"parent_net_id": "parent-1"})),
            created_by: Some("spawn:parent-1".to_string()),
            label: None,
            initial_tokens: Some(vec![InitialToken {
                place_id: "inbox".to_string(),
                token: serde_json::json!({"job_id": "j1", "spec": {}}),
                reply_routing: None,
            }]),
        };

        let json = serde_json::to_string(&request).unwrap();
        let parsed: CreateNetRequest = serde_json::from_str(&json).unwrap();

        let tokens = parsed.initial_tokens.unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].place_id, "inbox");
        assert_eq!(tokens[0].token["job_id"], "j1");
    }

    #[test]
    fn test_create_net_response() {
        let response = CreateNetResponse {
            success: true,
            net_id: "test-net".to_string(),
            error: None,
        };

        let json = serde_json::to_string(&response).unwrap();
        let parsed: CreateNetResponse = serde_json::from_str(&json).unwrap();

        assert!(parsed.success);
        assert_eq!(parsed.net_id, "test-net");
        assert!(parsed.error.is_none());
    }
}
