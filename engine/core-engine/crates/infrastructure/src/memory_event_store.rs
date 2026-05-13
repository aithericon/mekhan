use std::sync::RwLock;

use petri_application::{EventRepository, EventStoreError};
use petri_domain::{DomainEvent, PersistedEvent};

/// In-memory implementation of the event store.
/// Uses RwLock for thread-safe access.
pub struct MemoryEventStore {
    events: RwLock<Vec<PersistedEvent>>,
}

impl MemoryEventStore {
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
        }
    }

    /// Load an existing persisted event (e.g. from hydration).
    /// Does NOT recompute hash or sequence - trusts the input.
    pub fn load_existing_event(&self, event: PersistedEvent) {
        self.events.write().unwrap().push(event);
    }
}

impl Default for MemoryEventStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl EventRepository for MemoryEventStore {
    async fn append(&self, event: DomainEvent) -> Result<PersistedEvent, EventStoreError> {
        let mut events = self.events.write().unwrap();

        let sequence = events.len() as u64;
        let previous_hash = events.last().map(|e| e.hash.clone());

        let persisted = PersistedEvent::new(sequence, event, previous_hash);
        events.push(persisted.clone());

        Ok(persisted)
    }

    async fn all_events(&self) -> Vec<PersistedEvent> {
        self.events.read().unwrap().clone()
    }

    async fn events_since(&self, sequence: u64) -> Vec<PersistedEvent> {
        self.events
            .read()
            .unwrap()
            .iter()
            .filter(|e| e.sequence >= sequence)
            .cloned()
            .collect()
    }

    async fn reset(&self) {
        self.events.write().unwrap().clear();
    }

    async fn current_sequence(&self) -> u64 {
        self.events.read().unwrap().len() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_test_harness::prelude::*;

    #[rstest]
    #[tokio::test]
    async fn test_append_and_retrieve() {
        assert_append_and_retrieve(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_hash_chain_integrity() {
        assert_hash_chain_integrity(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_events_since() {
        assert_events_since(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_reset() {
        assert_reset(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_current_sequence() {
        assert_current_sequence(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_event_timestamps() {
        assert_event_timestamps(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_append_after_reset() {
        assert_append_after_reset(&MemoryEventStore::new()).await;
    }
}
