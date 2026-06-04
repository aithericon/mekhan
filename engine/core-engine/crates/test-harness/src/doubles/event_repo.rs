//! Mock EventRepository implementation for testing.

use parking_lot::RwLock;
use petri_application::{EventRepository, EventStoreError};
use petri_domain::{DomainEvent, PersistedEvent};

/// A configurable mock event repository for testing.
///
/// Features:
/// - Records all appended events for assertions
/// - Can be pre-populated with events
/// - Supports failure injection
/// - Thread-safe with RwLock
///
/// # Example
///
/// ```ignore
/// use petri_test_harness::prelude::*;
///
/// let repo = MockEventRepository::new();
/// repo.append(DomainEvent::ErrorOccurred { message: "test".into() });
///
/// assert_eq!(repo.append_count(), 1);
/// assert_eq!(repo.recorded_events().len(), 1);
/// ```
pub struct MockEventRepository {
    events: RwLock<Vec<PersistedEvent>>,
    /// Count of append calls (for verification)
    append_count: RwLock<usize>,
    /// Optional failure to inject on next append
    fail_on_append: RwLock<Option<String>>,
}

impl MockEventRepository {
    /// Create a new empty mock repository.
    pub fn new() -> Self {
        Self {
            events: RwLock::new(Vec::new()),
            append_count: RwLock::new(0),
            fail_on_append: RwLock::new(None),
        }
    }

    /// Create with pre-populated events.
    pub fn with_events(events: Vec<PersistedEvent>) -> Self {
        Self {
            events: RwLock::new(events),
            append_count: RwLock::new(0),
            fail_on_append: RwLock::new(None),
        }
    }

    /// Configure to fail on next append (for testing error handling).
    pub fn fail_next_append(&self, error: impl Into<String>) {
        *self.fail_on_append.write() = Some(error.into());
    }

    /// Get the number of times append was called.
    pub fn append_count(&self) -> usize {
        *self.append_count.read()
    }

    /// Get all recorded events (for assertions).
    pub fn recorded_events(&self) -> Vec<PersistedEvent> {
        self.events.read().clone()
    }

    /// Get the last recorded event.
    pub fn last_event(&self) -> Option<PersistedEvent> {
        self.events.read().last().cloned()
    }

    /// Check if any events were recorded.
    pub fn has_events(&self) -> bool {
        !self.events.read().is_empty()
    }

    /// Clear all recorded events and reset counters.
    pub fn clear(&self) {
        self.events.write().clear();
        *self.append_count.write() = 0;
    }
}

impl Default for MockEventRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl EventRepository for MockEventRepository {
    async fn append(&self, event: DomainEvent) -> Result<PersistedEvent, EventStoreError> {
        *self.append_count.write() += 1;

        // Check for injected failure
        if let Some(error) = self.fail_on_append.write().take() {
            return Err(EventStoreError::PersistFailed(error));
        }

        let mut events = self.events.write();
        let sequence = events.len() as u64;
        let previous_hash = events.last().map(|e| e.hash.clone());
        let persisted = PersistedEvent::new(sequence, event, previous_hash);
        events.push(persisted.clone());
        Ok(persisted)
    }

    async fn all_events(&self) -> Vec<PersistedEvent> {
        self.events.read().clone()
    }

    async fn events_since(&self, sequence: u64) -> Vec<PersistedEvent> {
        self.events
            .read()
            .iter()
            .filter(|e| e.sequence >= sequence)
            .cloned()
            .collect()
    }

    async fn reset(&self) {
        self.events.write().clear();
        *self.append_count.write() = 0;
    }

    async fn current_sequence(&self) -> u64 {
        self.events.read().len() as u64
    }

    // Override the trait defaults, which clone the whole log via `all_events()`
    // on every call. The default `events_from`/`len` make any code that reads
    // the marking once per step (the eval loop's `get_marking_cached`) O(n) per
    // step → O(n²) over a run, purely as a test-double artifact — the real
    // `MemoryEventStore` is O(1)/O(delta) here. Match it so the simulator (and
    // benchmarks built on it) reflect the engine's true per-step cost.
    async fn len(&self) -> usize {
        self.events.read().len()
    }

    async fn events_from(&self, idx: usize) -> Vec<PersistedEvent> {
        let events = self.events.read();
        let start = idx.min(events.len());
        events[start..].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_is_empty() {
        let repo = MockEventRepository::new();
        assert_eq!(repo.append_count(), 0);
        assert!(!repo.has_events());
    }

    #[tokio::test]
    async fn test_append_increments_count() {
        let repo = MockEventRepository::new();
        repo.append(DomainEvent::ErrorOccurred {
            message: "test".into(),
        })
        .await
        .unwrap();
        assert_eq!(repo.append_count(), 1);
        assert!(repo.has_events());
    }

    #[tokio::test]
    async fn test_recorded_events() {
        let repo = MockEventRepository::new();
        repo.append(DomainEvent::ErrorOccurred {
            message: "first".into(),
        })
        .await
        .unwrap();
        repo.append(DomainEvent::ErrorOccurred {
            message: "second".into(),
        })
        .await
        .unwrap();

        let events = repo.recorded_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].sequence, 0);
        assert_eq!(events[1].sequence, 1);
    }

    #[tokio::test]
    async fn test_fail_injection() {
        let repo = MockEventRepository::new();
        repo.fail_next_append("test error");
        let result = repo
            .append(DomainEvent::ErrorOccurred {
                message: "test".into(),
            })
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("test error"));
    }
}
