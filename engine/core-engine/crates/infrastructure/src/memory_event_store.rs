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

    // Storage-order count and positional slice. These are the correct
    // primitives for incremental cache cursoring — `current_sequence` /
    // `events_since` filter on the `.sequence` field, which is unsafe when
    // the cache holds hydrated events whose numbering restarts at 0 across
    // sessions (multi-session NATS streams).

    async fn len(&self) -> usize {
        self.events.read().unwrap().len()
    }

    async fn events_from(&self, idx: usize) -> Vec<PersistedEvent> {
        let events = self.events.read().unwrap();
        let start = idx.min(events.len());
        events[start..].to_vec()
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

    /// Index-based cursor (`len` + `events_from`) is safe when the cache
    /// holds hydrated events whose `.sequence` field restarts at 0 across
    /// sessions — the bug class behind `[[engine-loop-dup-seq]]`. The
    /// sequence-field-based pair (`current_sequence` / `events_since`) is
    /// *not* safe in that scenario; this test pins both behaviors.
    #[tokio::test]
    async fn cache_cursor_is_index_based_under_dup_sequences() {
        use petri_domain::{DomainEvent, PersistedEvent, PlaceId, Token, TokenColor};

        let store = MemoryEventStore::new();
        let place = PlaceId::named("p");

        // Simulate hydration of two sessions whose `.sequence` overlaps.
        // Each session starts fresh at 0; the second session's seq=0..2
        // events arrive AFTER the first session's seq=0..2 events.
        let make_token_created = |seq: u64| {
            PersistedEvent::new(
                seq,
                DomainEvent::TokenCreated {
                    token: Token::new(TokenColor::Unit),
                    place_id: place.clone(),
                    place_name: None,
                    workflow_id: None,
                    signal_key: None,
                    dedup_id: None,
                },
                None,
            )
        };
        // Session 1: seq 0..=2
        store.load_existing_event(make_token_created(0));
        store.load_existing_event(make_token_created(1));
        store.load_existing_event(make_token_created(2));
        // Session 2: seq 0..=2 again
        store.load_existing_event(make_token_created(0));
        store.load_existing_event(make_token_created(1));
        store.load_existing_event(make_token_created(2));

        // Storage holds 6 events, but `current_sequence` (which surfaces
        // `.sequence + 1`-style semantics via len) does NOT line up with
        // the per-event `.sequence` field once duplicates are present.
        assert_eq!(store.len().await, 6);

        // `events_since(3)` filters by `.sequence >= 3` — and finds NONE,
        // even though there are 3 events stored after position 3.
        let by_sequence_filter = store.events_since(3).await;
        assert_eq!(
            by_sequence_filter.len(),
            0,
            "events_since uses .sequence; with duplicate sequences it silently drops events"
        );

        // `events_from(3)` slices by storage position — finds the 3
        // events that were appended after cursor 3, regardless of their
        // `.sequence` field.
        let by_index_slice = store.events_from(3).await;
        assert_eq!(
            by_index_slice.len(),
            3,
            "events_from slices by index; safe under non-monotonic .sequence"
        );

        // Beyond end is a clamp, not a panic.
        assert_eq!(store.events_from(99).await.len(), 0);
    }
}
