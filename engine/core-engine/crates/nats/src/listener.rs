//! Token command listeners — inject, remove, update tokens via NATS.
//!
//! Each listener implements [`MessageHandler`] and delegates
//! to [`run_message_loop`] for the consume-parse-process-ack loop.

use std::sync::Arc;

use async_nats::jetstream::consumer::PullConsumer;
use async_nats::jetstream::Message;
use petri_application::{
    json_to_token_color, EventRepository, PetriNetService, StateProjection, TopologyRepository,
};
use petri_domain::PlaceId;
use serde::{Deserialize, Serialize};

use crate::dlq::DlqPublisher;
use crate::idempotency::{CachedResult, IdempotencyCache};
use crate::message_loop::{run_message_loop_cancellable, MessageHandler, ProcessError};

// ============================================================================
// Token Injection
// ============================================================================

/// Request to inject a token into a place.
///
/// Published to `petri.commands.inject.token` by external systems.
/// Idempotency is automatic via NATS message metadata (stream:sequence).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenInjectionRequest {
    /// Target place ID (scenario string ID, will be resolved to UUID)
    pub place_id: String,

    /// Token color/data to inject
    pub color: serde_json::Value,

    /// Optional correlation ID for tracing
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,

    /// Request timestamp (for debugging/tracing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

/// Unified response for token command operations (inject, remove, update).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenCommandResponse {
    /// Whether the command succeeded
    pub success: bool,

    /// Event sequence number if successful
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_sequence: Option<u64>,

    /// Token ID if successful
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,

    /// Error message if failed
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Correlation ID from request
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

/// Listens for token injection requests on NATS and creates tokens.
///
/// This enables external systems to inject tokens into the Petri net
/// without using the HTTP API.
pub struct TokenInjectionListener<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    /// The Petri net service to inject tokens into
    service: Arc<PetriNetService<E, T, S>>,

    /// NATS consumer for injection requests
    consumer: PullConsumer,

    /// Idempotency cache for deduplication
    idempotency_cache: Arc<IdempotencyCache>,

    /// Dead-letter publisher for unprocessable requests
    dlq: Option<DlqPublisher>,
}

impl<E, T, S> TokenInjectionListener<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    /// Create a new token injection listener.
    ///
    /// # Arguments
    /// * `service` - The Petri net service
    /// * `consumer` - JetStream consumer for `petri.commands.inject.token`
    pub fn new(service: Arc<PetriNetService<E, T, S>>, consumer: PullConsumer) -> Self {
        Self {
            service,
            consumer,
            idempotency_cache: Arc::new(IdempotencyCache::new()),
            dlq: None,
        }
    }

    /// Create a new token injection listener with a shared idempotency cache.
    ///
    /// Use this when you want to share the cache across multiple listeners.
    pub fn with_cache(
        service: Arc<PetriNetService<E, T, S>>,
        consumer: PullConsumer,
        idempotency_cache: Arc<IdempotencyCache>,
    ) -> Self {
        Self {
            service,
            consumer,
            idempotency_cache,
            dlq: None,
        }
    }

    /// Create a listener with restart-durable dedup and dead-lettering.
    ///
    /// The idempotency cache is backed by the `petri-idempotency` JetStream
    /// KV bucket (created if missing), and unprocessable requests land in
    /// the `PETRI_DLQ` stream instead of being dropped.
    pub async fn with_durable_cache(
        service: Arc<PetriNetService<E, T, S>>,
        consumer: PullConsumer,
        jetstream: &async_nats::jetstream::Context,
    ) -> Result<Self, ListenerError> {
        let cache = IdempotencyCache::durable(jetstream)
            .await
            .map_err(ListenerError::ServiceError)?;
        Ok(Self {
            service,
            consumer,
            idempotency_cache: Arc::new(cache),
            dlq: Some(DlqPublisher::new(jetstream.clone())),
        })
    }

    /// Attach a dead-letter publisher for unprocessable requests.
    pub fn with_dlq(mut self, dlq: DlqPublisher) -> Self {
        self.dlq = Some(dlq);
        self
    }

    /// Start listening for token injection requests.
    ///
    /// This runs until the consumer is closed or an unrecoverable error occurs.
    pub async fn run(self) -> Result<(), ListenerError> {
        let consumer = self.consumer.clone();
        run_message_loop_cancellable(consumer, &self, None, self.dlq.clone())
            .await
            .map_err(|e| ListenerError::ConsumerError(e.to_string()))
    }

    /// Handle a single injection request.
    ///
    /// The `idempotency_key` is derived automatically from NATS message metadata
    /// (stream:sequence), making deduplication transparent to clients.
    async fn handle_injection(
        &self,
        request: TokenInjectionRequest,
        idempotency_key: Option<&str>,
    ) -> TokenCommandResponse {
        // Check idempotency cache first (using automatic key from NATS metadata)
        if let Some(key) = idempotency_key {
            if let Some(cached) = self.idempotency_cache.get(key).await {
                tracing::debug!(
                    idempotency_key = %key,
                    "Returning cached response for duplicate injection request"
                );
                return match cached {
                    CachedResult::Success {
                        event_sequence,
                        token_id,
                    } => TokenCommandResponse {
                        success: true,
                        event_sequence: Some(event_sequence),
                        token_id,
                        error: None,
                        correlation_id: request.correlation_id,
                    },
                    CachedResult::Failure { error } => TokenCommandResponse {
                        success: false,
                        event_sequence: None,
                        token_id: None,
                        error: Some(error),
                        correlation_id: request.correlation_id,
                    },
                };
            }
        }

        // Resolve place ID (string IDs are the domain IDs now)
        let place_id = PlaceId(request.place_id.clone());

        // Convert JSON to TokenColor
        let color = json_to_token_color(&request.color);

        // Create the token
        match self.service.create_token(place_id, color).await {
            Ok(event) => {
                // Extract token ID from event
                let token_id =
                    if let petri_domain::DomainEvent::TokenCreated { token, .. } = &event.event {
                        Some(token.id.to_string())
                    } else {
                        None
                    };

                // Cache the success
                if let Some(key) = idempotency_key {
                    self.idempotency_cache
                        .insert(
                            key.to_string(),
                            CachedResult::Success {
                                event_sequence: event.sequence,
                                token_id: token_id.clone(),
                            },
                        )
                        .await;
                }

                TokenCommandResponse {
                    success: true,
                    event_sequence: Some(event.sequence),
                    token_id,
                    error: None,
                    correlation_id: request.correlation_id,
                }
            }
            Err(e) => {
                let error = e.to_string();
                // Cache the failure
                if let Some(key) = idempotency_key {
                    self.idempotency_cache
                        .insert(
                            key.to_string(),
                            CachedResult::Failure {
                                error: error.clone(),
                            },
                        )
                        .await;
                }
                TokenCommandResponse {
                    success: false,
                    event_sequence: None,
                    token_id: None,
                    error: Some(error),
                    correlation_id: request.correlation_id,
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl<E, T, S> MessageHandler for TokenInjectionListener<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    fn listener_name(&self) -> &str {
        "token-injection"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let request: TokenInjectionRequest =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        let correlation_id = request.correlation_id.clone();

        // Build automatic idempotency key from NATS message metadata
        let idempotency_key = msg
            .info()
            .ok()
            .map(|info| format!("{}:{}", info.stream, info.stream_sequence));

        tracing::debug!(
            place_id = %request.place_id,
            correlation_id = ?correlation_id,
            idempotency_key = ?idempotency_key,
            "Processing token injection request"
        );

        let response = self
            .handle_injection(request, idempotency_key.as_deref())
            .await;

        if response.success {
            tracing::info!(
                token_id = ?response.token_id,
                sequence = ?response.event_sequence,
                correlation_id = ?correlation_id,
                "Token injected successfully"
            );
            Ok(())
        } else {
            Err(ProcessError::Business(response.error.unwrap_or_default()))
        }
    }
}

/// Errors that can occur in token listeners.
#[derive(Debug, thiserror::Error)]
pub enum ListenerError {
    #[error("Consumer error: {0}")]
    ConsumerError(String),

    #[error("Service error: {0}")]
    ServiceError(String),
}

// ============================================================================
// Token Removal
// ============================================================================

/// Request to remove a token from a place.
///
/// Published to `petri.commands.remove.token` by external systems.
/// Used for job cancellation, resource destruction, etc.
/// Idempotency is automatic via NATS message metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRemovalRequest {
    /// Target place ID (scenario string ID, will be resolved to UUID)
    pub place_id: String,

    /// Specific token ID to remove (optional if correlation_id provided)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,

    /// Correlation ID to match in token data (optional if token_id provided)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,

    /// Reason for removal (for audit trail)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,

    /// Request timestamp (for debugging/tracing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

/// Listens for token removal requests on NATS.
pub struct TokenRemovalListener<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    service: Arc<PetriNetService<E, T, S>>,
    consumer: PullConsumer,
    dlq: Option<DlqPublisher>,
}

impl<E, T, S> TokenRemovalListener<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    pub fn new(service: Arc<PetriNetService<E, T, S>>, consumer: PullConsumer) -> Self {
        Self {
            service,
            consumer,
            dlq: None,
        }
    }

    /// Attach a dead-letter publisher for unprocessable requests.
    pub fn with_dlq(mut self, dlq: DlqPublisher) -> Self {
        self.dlq = Some(dlq);
        self
    }

    pub async fn run(self) -> Result<(), ListenerError> {
        let consumer = self.consumer.clone();
        run_message_loop_cancellable(consumer, &self, None, self.dlq.clone())
            .await
            .map_err(|e| ListenerError::ConsumerError(e.to_string()))
    }

    async fn handle_removal(&self, request: TokenRemovalRequest) -> TokenCommandResponse {
        // String IDs are the domain IDs now
        let place_id = PlaceId(request.place_id.clone());

        let token_id = request
            .token_id
            .as_ref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok().map(petri_domain::TokenId));

        match self
            .service
            .remove_token(
                place_id,
                token_id,
                request.correlation_id.clone(),
                request.reason,
            )
            .await
        {
            Ok(event) => {
                let removed_token_id = if let petri_domain::DomainEvent::TokenRemoved {
                    token_id,
                    ..
                } = &event.event
                {
                    Some(token_id.to_string())
                } else {
                    None
                };

                TokenCommandResponse {
                    success: true,
                    event_sequence: Some(event.sequence),
                    token_id: removed_token_id,
                    error: None,
                    correlation_id: request.correlation_id,
                }
            }
            Err(e) => TokenCommandResponse {
                success: false,
                event_sequence: None,
                token_id: None,
                error: Some(e.to_string()),
                correlation_id: request.correlation_id,
            },
        }
    }
}

#[async_trait::async_trait]
impl<E, T, S> MessageHandler for TokenRemovalListener<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    fn listener_name(&self) -> &str {
        "token-removal"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let request: TokenRemovalRequest =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        let correlation_id = request.correlation_id.clone();
        tracing::debug!(
            place_id = %request.place_id,
            correlation_id = ?correlation_id,
            "Processing token removal request"
        );

        let response = self.handle_removal(request).await;

        if response.success {
            tracing::info!(
                token_id = ?response.token_id,
                sequence = ?response.event_sequence,
                correlation_id = ?correlation_id,
                "Token removed successfully"
            );
            Ok(())
        } else {
            Err(ProcessError::Business(response.error.unwrap_or_default()))
        }
    }
}

// ============================================================================
// Token Update
// ============================================================================

/// Request to update a token's data in place.
///
/// Published to `petri.commands.update.token` by external systems.
/// Used for job priority changes, metadata updates, etc.
/// Idempotency is automatic via NATS message metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUpdateRequest {
    /// Target place ID (scenario string ID, will be resolved to UUID)
    pub place_id: String,

    /// Specific token ID to update (optional if correlation_id provided)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,

    /// Correlation ID to match in token data (optional if token_id provided)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,

    /// New token data
    pub new_color: serde_json::Value,

    /// Request timestamp (for debugging/tracing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<chrono::DateTime<chrono::Utc>>,
}

/// Listens for token update requests on NATS.
pub struct TokenUpdateListener<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    service: Arc<PetriNetService<E, T, S>>,
    consumer: PullConsumer,
    dlq: Option<DlqPublisher>,
}

impl<E, T, S> TokenUpdateListener<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    pub fn new(service: Arc<PetriNetService<E, T, S>>, consumer: PullConsumer) -> Self {
        Self {
            service,
            consumer,
            dlq: None,
        }
    }

    /// Attach a dead-letter publisher for unprocessable requests.
    pub fn with_dlq(mut self, dlq: DlqPublisher) -> Self {
        self.dlq = Some(dlq);
        self
    }

    pub async fn run(self) -> Result<(), ListenerError> {
        let consumer = self.consumer.clone();
        run_message_loop_cancellable(consumer, &self, None, self.dlq.clone())
            .await
            .map_err(|e| ListenerError::ConsumerError(e.to_string()))
    }

    async fn handle_update(&self, request: TokenUpdateRequest) -> TokenCommandResponse {
        // String IDs are the domain IDs now
        let place_id = PlaceId(request.place_id.clone());

        let token_id = request
            .token_id
            .as_ref()
            .and_then(|id| uuid::Uuid::parse_str(id).ok().map(petri_domain::TokenId));

        let new_color = json_to_token_color(&request.new_color);

        match self
            .service
            .update_token(
                place_id,
                token_id,
                request.correlation_id.clone(),
                new_color,
            )
            .await
        {
            Ok(event) => {
                let updated_token_id = if let petri_domain::DomainEvent::TokenUpdated {
                    token_id,
                    ..
                } = &event.event
                {
                    Some(token_id.to_string())
                } else {
                    None
                };

                TokenCommandResponse {
                    success: true,
                    event_sequence: Some(event.sequence),
                    token_id: updated_token_id,
                    error: None,
                    correlation_id: request.correlation_id,
                }
            }
            Err(e) => TokenCommandResponse {
                success: false,
                event_sequence: None,
                token_id: None,
                error: Some(e.to_string()),
                correlation_id: request.correlation_id,
            },
        }
    }
}

#[async_trait::async_trait]
impl<E, T, S> MessageHandler for TokenUpdateListener<E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    fn listener_name(&self) -> &str {
        "token-update"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let request: TokenUpdateRequest =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        let correlation_id = request.correlation_id.clone();
        tracing::debug!(
            place_id = %request.place_id,
            correlation_id = ?correlation_id,
            "Processing token update request"
        );

        let response = self.handle_update(request).await;

        if response.success {
            tracing::info!(
                token_id = ?response.token_id,
                sequence = ?response.event_sequence,
                correlation_id = ?correlation_id,
                "Token updated successfully"
            );
            Ok(())
        } else {
            Err(ProcessError::Business(response.error.unwrap_or_default()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_injection_request_deserialize() {
        let json = r#"{
            "place_id": "p_tasks",
            "color": {"id": "task-1", "priority": 5},
            "correlation_id": "req-123"
        }"#;

        let request: TokenInjectionRequest = serde_json::from_str(json).unwrap();
        assert_eq!(request.place_id, "p_tasks");
        assert_eq!(request.correlation_id, Some("req-123".to_string()));
    }
}
