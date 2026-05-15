//! Mock NATS publisher for unit testing NATS integration code.

use std::sync::Arc;

use parking_lot::RwLock;
use petri_application::{EventRepository, EventStoreError};
use petri_domain::{DomainEvent, PersistedEvent};

/// Mock NATS publisher that records published events without network.
///
/// Useful for unit testing NATS integration code without requiring
/// a running NATS server.
///
/// # Example
///
/// ```ignore
/// use petri_test_harness::prelude::*;
/// use petri_test_harness::nats::MockNatsPublisher;
///
/// let inner = Arc::new(MockEventRepository::new());
/// let publisher = MockNatsPublisher::new(inner);
///
/// publisher.append(DomainEvent::ErrorOccurred { message: "test".into() });
///
/// assert_eq!(publisher.publish_count(), 1);
/// let (subject, _payload) = &publisher.published_messages()[0];
/// assert!(subject.starts_with("petri.events."));
/// ```
pub struct MockNatsPublisher<E: EventRepository> {
    inner: Arc<E>,
    /// (subject, payload) pairs for all published messages
    published: RwLock<Vec<(String, Vec<u8>)>>,
}

impl<E: EventRepository> MockNatsPublisher<E> {
    /// Create a new mock publisher wrapping an inner repository.
    pub fn new(inner: Arc<E>) -> Self {
        Self {
            inner,
            published: RwLock::new(Vec::new()),
        }
    }

    /// Get all published messages for assertions.
    pub fn published_messages(&self) -> Vec<(String, Vec<u8>)> {
        self.published.read().clone()
    }

    /// Get count of published messages.
    pub fn publish_count(&self) -> usize {
        self.published.read().len()
    }

    /// Clear published messages.
    pub fn clear_published(&self) {
        self.published.write().clear();
    }

    /// Get the inner repository.
    pub fn inner(&self) -> &Arc<E> {
        &self.inner
    }
}

#[async_trait::async_trait]
impl<E: EventRepository + 'static> EventRepository for MockNatsPublisher<E> {
    async fn append(&self, event: DomainEvent) -> Result<PersistedEvent, EventStoreError> {
        let persisted = self.inner.append(event).await?;

        // Simulate NATS publish
        let subject = format!("petri.events.{}", event_type_name(&persisted.event));
        let payload = serde_json::to_vec(&persisted).unwrap_or_default();

        self.published.write().push((subject, payload));

        Ok(persisted)
    }

    async fn all_events(&self) -> Vec<PersistedEvent> {
        self.inner.all_events().await
    }

    async fn events_since(&self, sequence: u64) -> Vec<PersistedEvent> {
        self.inner.events_since(sequence).await
    }

    async fn reset(&self) {
        self.inner.reset().await;
        self.clear_published();
    }

    async fn current_sequence(&self) -> u64 {
        self.inner.current_sequence().await
    }
}

/// Get a string representation of an event type for NATS subject naming.
fn event_type_name(event: &DomainEvent) -> &'static str {
    match event {
        DomainEvent::NetInitialized { .. } => "net.initialized",
        DomainEvent::TokenCreated { .. } => "token.created",
        DomainEvent::TransitionFired { .. } => "transition.fired",
        DomainEvent::TokenConsumed { .. } => "token.consumed",
        DomainEvent::TokenRemoved { .. } => "token.removed",
        DomainEvent::TokenUpdated { .. } => "token.updated",
        DomainEvent::ErrorOccurred { .. } => "error",
        DomainEvent::TokenBridgedOut { .. } => "token.bridged_out",
        DomainEvent::TransitionScriptUpdated { .. } => "transition.script_updated",
        DomainEvent::EffectCompleted { .. } => "effect.completed",
        DomainEvent::EffectFailed { .. } => "effect.failed",
        DomainEvent::NetCreated { .. } => "net.created",
        DomainEvent::NetCompleted { .. } => "net.completed",
        DomainEvent::NetCancelled { .. } => "net.cancelled",
        DomainEvent::PreDispatchEvaluated { .. } => "pre_dispatch.evaluated",
        DomainEvent::PreDispatchRejected { .. } => "pre_dispatch.rejected",
        DomainEvent::PreDispatchDeferred { .. } => "pre_dispatch.deferred",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doubles::MockEventRepository;

    #[tokio::test]
    async fn test_publish_records_message() {
        let inner = Arc::new(MockEventRepository::new());
        let publisher = MockNatsPublisher::new(inner);

        publisher
            .append(DomainEvent::ErrorOccurred {
                message: "test".into(),
            })
            .await
            .unwrap();

        assert_eq!(publisher.publish_count(), 1);
        let (subject, payload) = &publisher.published_messages()[0];
        assert_eq!(subject, "petri.events.error");
        assert!(!payload.is_empty());
    }

    #[tokio::test]
    async fn test_publish_different_event_types() {
        let inner = Arc::new(MockEventRepository::new());
        let publisher = MockNatsPublisher::new(inner);

        publisher
            .append(DomainEvent::ErrorOccurred {
                message: "test".into(),
            })
            .await
            .unwrap();

        // Create a dummy TokenCreated event
        let place_id = petri_domain::PlaceId::new();
        let token = petri_domain::Token::new_unit();
        publisher
            .append(DomainEvent::TokenCreated {
                place_id,
                token,
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            })
            .await
            .unwrap();

        let messages = publisher.published_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].0, "petri.events.error");
        assert_eq!(messages[1].0, "petri.events.token.created");
    }

    #[tokio::test]
    async fn test_reset_clears_published() {
        let inner = Arc::new(MockEventRepository::new());
        let publisher = MockNatsPublisher::new(inner);

        publisher
            .append(DomainEvent::ErrorOccurred {
                message: "test".into(),
            })
            .await
            .unwrap();
        assert_eq!(publisher.publish_count(), 1);

        publisher.reset().await;
        assert_eq!(publisher.publish_count(), 0);
        assert_eq!(publisher.all_events().await.len(), 0);
    }
}
