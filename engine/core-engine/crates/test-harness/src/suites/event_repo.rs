//! Generic test suite for EventRepository implementations.
//!
//! This module provides test functions that can be used with rstest to validate
//! any EventRepository implementation.
//!
//! # Usage with rstest
//!
//! ```ignore
//! use rstest::rstest;
//! use petri_test_harness::suites::event_repo::*;
//!
//! #[rstest]
//! #[tokio::test]
//! async fn test_append(repo: impl EventRepository) {
//!     assert_append_and_retrieve(&repo).await;
//! }
//! ```

use petri_application::EventRepository;
use petri_domain::{verify_event_chain, DomainEvent};

/// Assert that append creates events with correct sequences and hash chaining.
pub async fn assert_append_and_retrieve(repo: &impl EventRepository) {
    let event1 = repo
        .append(DomainEvent::ErrorOccurred {
            message: "test1".to_string(),
        })
        .await
        .unwrap();
    let event2 = repo
        .append(DomainEvent::ErrorOccurred {
            message: "test2".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(event1.sequence, 0, "First event should have sequence 0");
    assert_eq!(event2.sequence, 1, "Second event should have sequence 1");
    assert!(
        event1.previous_hash.is_none(),
        "First event should have no previous hash"
    );
    assert_eq!(
        event2.previous_hash,
        Some(event1.hash.clone()),
        "Second event should chain to first"
    );

    let all = repo.all_events().await;
    assert_eq!(all.len(), 2, "Should have 2 events");
    assert_eq!(all[0].sequence, 0);
    assert_eq!(all[1].sequence, 1);
}

/// Assert that the hash chain is valid after multiple appends.
pub async fn assert_hash_chain_integrity(repo: &impl EventRepository) {
    for i in 0..10 {
        repo.append(DomainEvent::ErrorOccurred {
            message: format!("event {}", i),
        })
        .await
        .unwrap();
    }

    let events = repo.all_events().await;
    assert_eq!(events.len(), 10, "Should have 10 events");
    assert!(verify_event_chain(&events), "Event chain should be valid");
}

/// Assert that events_since returns the correct subset.
pub async fn assert_events_since(repo: &impl EventRepository) {
    for i in 0..5 {
        repo.append(DomainEvent::ErrorOccurred {
            message: format!("event {}", i),
        })
        .await
        .unwrap();
    }

    let since_0 = repo.events_since(0).await;
    assert_eq!(since_0.len(), 5, "events_since(0) should return all events");

    let since_3 = repo.events_since(3).await;
    assert_eq!(since_3.len(), 2, "events_since(3) should return 2 events");
    assert_eq!(since_3[0].sequence, 3);
    assert_eq!(since_3[1].sequence, 4);

    let since_5 = repo.events_since(5).await;
    assert_eq!(since_5.len(), 0, "events_since(5) should return empty");
}

/// Assert that reset clears all events.
pub async fn assert_reset(repo: &impl EventRepository) {
    repo.append(DomainEvent::ErrorOccurred {
        message: "test".to_string(),
    })
    .await
    .unwrap();
    repo.append(DomainEvent::ErrorOccurred {
        message: "test2".to_string(),
    })
    .await
    .unwrap();
    assert_eq!(repo.all_events().await.len(), 2);

    repo.reset().await;

    assert_eq!(repo.all_events().await.len(), 0, "Events should be cleared");
    assert_eq!(
        repo.current_sequence().await,
        0,
        "Sequence should reset to 0"
    );
}

/// Assert that current_sequence tracks correctly.
pub async fn assert_current_sequence(repo: &impl EventRepository) {
    assert_eq!(
        repo.current_sequence().await,
        0,
        "Initial sequence should be 0"
    );

    repo.append(DomainEvent::ErrorOccurred {
        message: "1".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        repo.current_sequence().await,
        1,
        "Sequence should be 1 after first append"
    );

    repo.append(DomainEvent::ErrorOccurred {
        message: "2".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        repo.current_sequence().await,
        2,
        "Sequence should be 2 after second append"
    );
}

/// Assert that events are persisted with correct timestamps.
pub async fn assert_event_timestamps(repo: &impl EventRepository) {
    let before = chrono::Utc::now();

    repo.append(DomainEvent::ErrorOccurred {
        message: "test".to_string(),
    })
    .await
    .unwrap();

    let after = chrono::Utc::now();

    let events = repo.all_events().await;
    let event = &events[0];

    assert!(
        event.timestamp >= before && event.timestamp <= after,
        "Event timestamp should be between before and after"
    );
}

/// Assert that appending after reset starts fresh.
pub async fn assert_append_after_reset(repo: &impl EventRepository) {
    repo.append(DomainEvent::ErrorOccurred {
        message: "before".to_string(),
    })
    .await
    .unwrap();
    repo.reset().await;

    let event = repo
        .append(DomainEvent::ErrorOccurred {
            message: "after".to_string(),
        })
        .await
        .unwrap();

    assert_eq!(
        event.sequence, 0,
        "Sequence should restart at 0 after reset"
    );
    assert!(
        event.previous_hash.is_none(),
        "First event after reset should have no previous hash"
    );
}

/// Run all EventRepository assertions against an implementation.
///
/// This is a convenience function that runs all tests in sequence.
/// Prefer using rstest with individual assertions for better test isolation.
pub async fn assert_all(repo: &impl EventRepository) {
    assert_append_and_retrieve(repo).await;
    repo.reset().await;

    assert_hash_chain_integrity(repo).await;
    repo.reset().await;

    assert_events_since(repo).await;
    repo.reset().await;

    assert_reset(repo).await;
    // reset already called by assert_reset

    assert_current_sequence(repo).await;
    repo.reset().await;

    assert_event_timestamps(repo).await;
    repo.reset().await;

    assert_append_after_reset(repo).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doubles::MockEventRepository;
    use rstest::rstest;

    // Use rstest to run all assertions against MockEventRepository
    #[rstest]
    #[tokio::test]
    async fn test_append_and_retrieve() {
        assert_append_and_retrieve(&MockEventRepository::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_hash_chain_integrity() {
        assert_hash_chain_integrity(&MockEventRepository::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_events_since() {
        assert_events_since(&MockEventRepository::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_reset() {
        assert_reset(&MockEventRepository::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_current_sequence() {
        assert_current_sequence(&MockEventRepository::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_event_timestamps() {
        assert_event_timestamps(&MockEventRepository::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_append_after_reset() {
        assert_append_after_reset(&MockEventRepository::new()).await;
    }
}
