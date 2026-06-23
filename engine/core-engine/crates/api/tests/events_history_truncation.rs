//! FIX 3 (MAJOR 4) regression — bounded `get_events()` history/backfill must
//! not silently truncate the external `/api/events` contract.
//!
//! Background: the bounded-rehydration change made `service.get_events()`
//! return only the resident **tail** of the bounded `MemoryEventStore`, not the
//! full history. Two downstream regressions followed in the HTTP layer:
//!
//!   1. The GET `/api/events` handler ran the genesis-anchored
//!      `verify_event_chain` over the partial tail. Because the tail's first
//!      event is no longer genesis (`sequence > 0`, `previous_hash = Some(_)`),
//!      a perfectly valid contiguous tail was misreported as `chain_valid:
//!      false`.
//!   2. A `?from_sequence=N` request below the evicted floor silently returned
//!      only the recent tail, with the middle range dropped and NO signal that
//!      the in-memory view was partial.
//!
//! These tests drive the REAL `/api/events` handler (via `create_router`) over
//! a REAL bounded [`MemoryEventStore`] whose prefix has been evicted, and
//! assert the post-fix contract:
//!   - `chain_valid` is `true` for the valid contiguous tail (no false break),
//!   - `history_truncated` is `true` with a correct `earliest_available_sequence`
//!     when the request floor is below what memory holds,
//!   - the returned events are exactly the contiguous tail the store still has.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use parking_lot::RwLock;
use petri_api::router::AppState;
use petri_api::create_router;
use petri_api_types::EventsResponse;
use petri_application::{AdapterScheduler, EventRepository, PetriNetService};
use petri_domain::DomainEvent;
use petri_infrastructure::MemoryEventStore;
use petri_test_harness::doubles::{MockStateProjection, MockTopologyRepository};
use tokio::sync::Notify;
use tower::ServiceExt;

/// A fat `ErrorOccurred` event whose JSON payload is large enough that only a
/// handful fit under the tail cap — guarantees prefix eviction.
fn fat_event(i: u64) -> DomainEvent {
    DomainEvent::ErrorOccurred {
        // ~4 KiB payload so a 16 KiB tail cap holds only the last few events.
        message: format!("event-{i}-{}", "x".repeat(4096)),
    }
}

/// Build an `AppState` whose event store is a REAL bounded `MemoryEventStore`
/// with a tiny tail cap, primed with `n` hash-chained events so the prefix is
/// evicted. Returns `(app_state, store)` so the test can read the evicted
/// floor directly off the store.
async fn primed_bounded_state(
    n: u64,
    tail_cap_bytes: usize,
) -> (
    AppState<MemoryEventStore, MockTopologyRepository, MockStateProjection>,
    Arc<MemoryEventStore>,
) {
    let store = Arc::new(MemoryEventStore::with_tail_cap(tail_cap_bytes));
    for i in 0..n {
        // `append` builds a correctly hash-chained PersistedEvent (sequence
        // from next_sequence, previous_hash from the current tip) and evicts
        // down to the byte cap — exactly the production append path.
        store.append(fat_event(i)).await.expect("append");
    }

    let service = Arc::new(PetriNetService::new(
        store.clone(),
        Arc::new(MockTopologyRepository::new()),
        Arc::new(MockStateProjection::new()),
    ));
    let (event_tx, _) = tokio::sync::broadcast::channel(256);

    let app_state = AppState {
        service,
        adapter_scheduler: Arc::new(AdapterScheduler::new()),
        run_mode: Arc::new(RwLock::new(petri_api::dto::RunMode::default())),
        eval_notify: Arc::new(Notify::new()),
        event_tx: Arc::new(event_tx),
        dispatch_options: Arc::new(RwLock::new(petri_domain::DispatchOptions::default())),
    };
    (app_state, store)
}

fn router(
    app_state: AppState<MemoryEventStore, MockTopologyRepository, MockStateProjection>,
) -> Router {
    Router::new().nest("/api", create_router(app_state))
}

async fn get_events(router: &Router, query: &str) -> EventsResponse {
    let uri = format!("/api/events{query}");
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("EventsResponse")
}

#[tokio::test]
async fn get_events_evicted_tail_is_not_false_flagged_and_marks_truncation() {
    // 64 fat (~4 KiB) events into a 16 KiB tail cap → most of the prefix is
    // evicted; only a handful of the most-recent events stay resident.
    let total: u64 = 64;
    let (app_state, store) = primed_bounded_state(total, 16 * 1024).await;

    // Sanity: the store really did evict a prefix.
    let earliest = store
        .earliest_available_sequence()
        .await
        .expect("tail is non-empty");
    assert!(
        earliest > 0,
        "expected prefix eviction (earliest_available_sequence > 0), got {earliest}"
    );

    let router = router(app_state);

    // --- Unfiltered request (implicit from_sequence = 0, below the floor) ----
    let resp = get_events(&router, "").await;

    // Pre-fix bug #1: the partial tail was verified with the genesis-anchored
    // verifier and reported chain_valid = false. Post-fix: a valid contiguous
    // tail is verified with verify_event_chain_from and reports true.
    assert!(
        resp.chain_valid,
        "valid contiguous tail must NOT be reported as a broken chain"
    );

    // Pre-fix bug #2: no signal that the history was partial. Post-fix: the
    // response carries an explicit truncation marker + the floor.
    assert_eq!(resp.earliest_available_sequence, Some(earliest));
    assert!(
        resp.history_truncated,
        "unfiltered request below the evicted floor must be flagged truncated"
    );

    // The returned events ARE exactly the resident tail, contiguous, ending at
    // the last appended sequence.
    assert_eq!(resp.events.first().unwrap().sequence, earliest);
    assert_eq!(resp.events.last().unwrap().sequence, total - 1);
    for w in resp.events.windows(2) {
        assert_eq!(w[1].sequence, w[0].sequence + 1, "contiguous tail");
    }

    // --- Request from BELOW the evicted floor ------------------------------
    // A client reconnecting and asking for an evicted range learns the range
    // is truncated and where the in-memory window begins.
    let below = earliest - 1;
    let resp = get_events(&router, &format!("?from_sequence={below}")).await;
    assert!(resp.chain_valid);
    assert!(
        resp.history_truncated,
        "from_sequence below the floor must be flagged truncated"
    );
    assert_eq!(resp.earliest_available_sequence, Some(earliest));
    // The contiguous tail is still returned (earliest..=total-1), not an empty
    // or middle-dropped slice.
    assert_eq!(resp.events.first().unwrap().sequence, earliest);
    assert_eq!(resp.events.last().unwrap().sequence, total - 1);

    // --- Request AT/above the floor is NOT truncated -----------------------
    let resp = get_events(&router, &format!("?from_sequence={earliest}")).await;
    assert!(resp.chain_valid);
    assert!(
        !resp.history_truncated,
        "from_sequence at the floor is fully served — not truncated"
    );
    assert_eq!(resp.events.first().unwrap().sequence, earliest);
}

#[tokio::test]
async fn sse_backfill_below_floor_emits_history_truncated_frame() {
    // The SSE backfill (GET /api/events/stream?from_sequence=N) previously
    // filtered the bounded tail and silently dropped any evicted middle range.
    // Post-fix: when the requested range starts below the resident floor it
    // emits an explicit `history_truncated` SSE frame up front so a reconnecting
    // client knows its backfill is partial.
    let total: u64 = 64;
    let (app_state, store) = primed_bounded_state(total, 16 * 1024).await;
    let earliest = store.earliest_available_sequence().await.unwrap();
    assert!(earliest > 0, "expected prefix eviction");

    let router = router(app_state);

    let resp = router
        .oneshot(
            Request::builder()
                .uri(format!("/api/events/stream?from_sequence={}", earliest - 1))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // The backfill frames are emitted immediately; the live phase then blocks.
    // Read a bounded prefix of the stream with a timeout and parse SSE frames.
    let mut body = resp.into_body().into_data_stream();
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    use futures::StreamExt;
    // Collect until we've seen the full backfill (the last tail event) or time out.
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(250), body.next()).await {
            Ok(Some(Ok(chunk))) => {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                if buf.contains(&format!("\"sequence\":{}", total - 1)) {
                    break;
                }
            }
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => {
                // Idle (live phase blocking) — backfill is done.
                if !buf.is_empty() {
                    break;
                }
            }
        }
    }

    // The truncation frame is present and carries the floor.
    assert!(
        buf.contains("event: history_truncated"),
        "expected a history_truncated SSE frame, got:\n{buf}"
    );
    assert!(
        buf.contains(&format!("\"earliest_available_sequence\":{earliest}")),
        "truncation frame must carry the floor, got:\n{buf}"
    );
    // And the resident tail is still backfilled (last event present).
    assert!(
        buf.contains(&format!("\"sequence\":{}", total - 1)),
        "backfill must still include the resident tail"
    );
}

#[tokio::test]
async fn get_events_full_retention_never_truncates() {
    // A huge tail cap retains everything: genesis stays resident, so the
    // history is never truncated and the genesis-anchored chain is valid.
    let total: u64 = 8;
    let (app_state, _store) = primed_bounded_state(total, usize::MAX).await;
    let router = router(app_state);

    let resp = get_events(&router, "").await;
    assert!(resp.chain_valid);
    assert!(!resp.history_truncated);
    assert_eq!(resp.earliest_available_sequence, Some(0));
    assert_eq!(resp.events.first().unwrap().sequence, 0);
    assert_eq!(resp.events.last().unwrap().sequence, total - 1);
}
