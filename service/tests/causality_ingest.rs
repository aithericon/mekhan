//! Integration tests for the causality ingest consumer.
//!
//! Validates that domain events from PETRI_GLOBAL are correctly projected
//! into the causality tables with proper process tag propagation.
//!
//! Requires: `just -f aithericon-test-infra/justfile up` (Postgres + NATS)

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::stream::Config as StreamConfig;
use serde_json::json;
use uuid::Uuid;

use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::causality::ingest::start_causality_ingest;
use mekhan_service::causality::live::LiveBroadcasts;
use mekhan_service::nats::MekhanNats;

// ── Helpers ────────────────────────────────────────────────────────────────

async fn ensure_petri_global_stream(js: &jetstream::Context) {
    js.get_or_create_stream(StreamConfig {
        name: "PETRI_GLOBAL".to_string(),
        subjects: vec!["petri.>".to_string()],
        max_age: Duration::from_secs(300),
        ..Default::default()
    })
    .await
    .expect("create PETRI_GLOBAL stream");
}

/// Build a PersistedEvent JSON envelope.
fn persisted_event(sequence: u64, event: serde_json::Value) -> serde_json::Value {
    json!({
        "sequence": sequence,
        "timestamp": "2026-04-04T12:00:00Z",
        "event": event,
        "hash": format!("fake-hash-{sequence}"),
        "previous_hash": if sequence > 1 { Some(format!("fake-hash-{}", sequence - 1)) } else { None }
    })
}

/// Publish a PersistedEvent to the correct NATS subject.
async fn publish_event(
    js: &jetstream::Context,
    net_id: &str,
    event_suffix: &str,
    payload: &serde_json::Value,
) {
    let subject = format!("petri.events.{net_id}.{event_suffix}");
    let bytes = serde_json::to_vec(payload).unwrap();
    js.publish(subject, bytes.into())
        .await
        .expect("publish event")
        .await
        .expect("event ACK");
}

/// Publish a CrossNetTokenTransfer to the bridge subject.
async fn publish_bridge_transfer(
    js: &jetstream::Context,
    target_net_id: &str,
    place: &str,
    payload: &serde_json::Value,
) {
    let subject = format!("petri.bridge.{target_net_id}.{place}");
    let bytes = serde_json::to_vec(payload).unwrap();
    js.publish(subject, bytes.into())
        .await
        .expect("publish bridge transfer")
        .await
        .expect("bridge transfer ACK");
}

/// Wait for a causality_events row to appear.
async fn wait_for_causality_event(
    db: &sqlx::PgPool,
    net_id: &str,
    event_seq: i64,
    timeout: Duration,
) {
    let start = std::time::Instant::now();
    loop {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM causality_events WHERE net_id = $1 AND event_seq = $2)",
        )
        .bind(net_id)
        .bind(event_seq)
        .fetch_one(db)
        .await
        .unwrap_or(false);

        if exists {
            return;
        }
        if start.elapsed() > timeout {
            panic!(
                "causality_events row ({net_id}, {event_seq}) did not appear within {timeout:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait for a causality_cross_links row to appear.
async fn wait_for_cross_link(db: &sqlx::PgPool, signal_key: &str, timeout: Duration) {
    let start = std::time::Instant::now();
    loop {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM causality_cross_links WHERE signal_key = $1)",
        )
        .bind(signal_key)
        .fetch_one(db)
        .await
        .unwrap_or(false);

        if exists {
            return;
        }
        if start.elapsed() > timeout {
            panic!(
                "causality_cross_links row ({signal_key}) did not appear within {timeout:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait for a causality_process_tags row to appear.
async fn wait_for_process_tag(db: &sqlx::PgPool, token_id: &str, timeout: Duration) {
    let start = std::time::Instant::now();
    loop {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM causality_process_tags WHERE token_id = $1)",
        )
        .bind(token_id)
        .fetch_one(db)
        .await
        .unwrap_or(false);

        if exists {
            return;
        }
        if start.elapsed() > timeout {
            panic!(
                "causality_process_tags row (token={token_id}) did not appear within {timeout:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Handle for a running causality ingest task. Aborts on drop.
struct IngestHandle(tokio::task::AbortHandle);
impl Drop for IngestHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Start the causality ingest consumer in the background.
///
/// Returns a handle that aborts the task on drop, preventing cross-test interference.
async fn spawn_causality_ingest(nats: &MekhanNats, db: &sqlx::PgPool) -> IngestHandle {
    // Clean slate: delete the consumer, purge the stream, then recreate.
    // This ensures no stale messages from prior tests are replayed.
    if let Ok(stream) = nats.jetstream().get_stream("PETRI_GLOBAL").await {
        let _ = stream.delete_consumer("mekhan-causality-ingest").await;
        let _ = stream.purge().await;
    }
    // Wait for deletion to propagate and old consumer tasks to notice
    tokio::time::sleep(Duration::from_millis(200)).await;

    let nats = nats.clone();
    let db = db.clone();
    let kv = nats.ensure_catalogue_subscriptions_kv().await.expect("create KV");
    let sub_mgr = Arc::new(SubscriptionManager::new(kv, nats.jetstream().clone()));
    let live = LiveBroadcasts::new();
    let handle = tokio::spawn(async move {
        start_causality_ingest(nats, db, sub_mgr, live).await;
    });
    // Give consumer time to subscribe
    tokio::time::sleep(Duration::from_millis(300)).await;
    IngestHandle(handle.abort_handle())
}

// ── Token helpers ──────────────────────────────────────────────────────────

fn token_json(id: &str) -> serde_json::Value {
    json!({
        "id": id,
        "color": { "type": "Unit" },
        "created_at": "2026-04-04T12:00:00Z"
    })
}

fn token_created_event(token_id: &str, place_id: &str) -> serde_json::Value {
    json!({
        "type": "TokenCreated",
        "token": token_json(token_id),
        "place_id": place_id,
        "place_name": "test_place"
    })
}

fn token_created_with_signal_key(token_id: &str, place_id: &str, signal_key: &str) -> serde_json::Value {
    json!({
        "type": "TokenCreated",
        "token": token_json(token_id),
        "place_id": place_id,
        "place_name": "test_place",
        "signal_key": signal_key
    })
}

fn transition_fired_event(
    transition_name: &str,
    consumed: &[(&str, &str)],  // (place_id, token_id)
    produced: &[(&str, &str, serde_json::Value)], // (place_id, token_id, token_json)
) -> serde_json::Value {
    let consumed_tokens: Vec<serde_json::Value> = consumed
        .iter()
        .map(|(place, tid)| json!([place, tid]))
        .collect();
    let produced_tokens: Vec<serde_json::Value> = produced
        .iter()
        .map(|(place, _tid, token)| json!([place, token]))
        .collect();

    json!({
        "type": "TransitionFired",
        "transition_id": Uuid::new_v4().to_string(),
        "transition_name": transition_name,
        "consumed_tokens": consumed_tokens,
        "produced_tokens": produced_tokens
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn token_created_seeds_process() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&common::nats_url(), None)
        .await
        .expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;
    let _handle = spawn_causality_ingest(&nats, &db).await;

    let net_id = format!("test-{}", Uuid::new_v4().simple());
    let token_id = Uuid::new_v4().to_string();
    let place_id = Uuid::new_v4().to_string();

    let event = persisted_event(1, token_created_event(&token_id, &place_id));
    publish_event(nats.jetstream(), &net_id, "token.created", &event).await;

    // Wait for full processing (process tag is the last thing written)
    wait_for_process_tag(&db, &token_id, Duration::from_secs(5)).await;

    // Assert causality_events
    let event_type: String = sqlx::query_scalar(
        "SELECT event_type FROM causality_events WHERE net_id = $1 AND event_seq = 1",
    )
    .bind(&net_id)
    .fetch_one(&db)
    .await
    .expect("fetch event type");
    assert_eq!(event_type, "TokenCreated");

    // Assert causality_event_tokens (produced)
    let token_role: String = sqlx::query_scalar(
        "SELECT role FROM causality_event_tokens WHERE net_id = $1 AND event_seq = 1 AND token_id = $2",
    )
    .bind(&net_id)
    .bind(&token_id)
    .fetch_one(&db)
    .await
    .expect("fetch token role");
    assert_eq!(token_role, "produced");

    // Assert self-tag in causality_process_tags
    let process_tag: String = sqlx::query_scalar(
        "SELECT process_id FROM causality_process_tags WHERE token_id = $1",
    )
    .bind(&token_id)
    .fetch_one(&db)
    .await
    .expect("fetch process tag");
    assert_eq!(process_tag, token_id, "seed token should self-tag");

    // Assert hpi_processes auto-created
    let proc_status: String = sqlx::query_scalar(
        "SELECT status FROM hpi_processes WHERE process_id = $1",
    )
    .bind(&token_id)
    .fetch_one(&db)
    .await
    .expect("fetch process status");
    assert_eq!(proc_status, "active");
}

#[tokio::test]
async fn transition_fired_propagates_tags() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&common::nats_url(), None)
        .await
        .expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;
    let _handle = spawn_causality_ingest(&nats, &db).await;

    let net_id = format!("test-{}", Uuid::new_v4().simple());
    let token_a = Uuid::new_v4().to_string();
    let token_b = Uuid::new_v4().to_string();
    let place_in = Uuid::new_v4().to_string();
    let place_out = Uuid::new_v4().to_string();

    // 1. Publish TokenCreated for token-A (seeds process)
    let ev1 = persisted_event(1, token_created_event(&token_a, &place_in));
    publish_event(nats.jetstream(), &net_id, "token.created", &ev1).await;
    wait_for_process_tag(&db, &token_a, Duration::from_secs(5)).await;

    // 2. Publish TransitionFired consuming A, producing B
    let ev2 = persisted_event(
        2,
        transition_fired_event(
            "transform",
            &[(&place_in, &token_a)],
            &[(&place_out, &token_b, token_json(&token_b))],
        ),
    );
    publish_event(nats.jetstream(), &net_id, "transition.fired", &ev2).await;
    wait_for_process_tag(&db, &token_b, Duration::from_secs(5)).await;

    // Assert: token-B inherited process tag from token-A
    let tag: String = sqlx::query_scalar(
        "SELECT process_id FROM causality_process_tags WHERE token_id = $1",
    )
    .bind(&token_b)
    .fetch_one(&db)
    .await
    .expect("token-B should have process tag");
    assert_eq!(tag, token_a, "token-B should inherit token-A's process");

    // Assert: consumed and produced token rows
    let consumed_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM causality_event_tokens \
         WHERE net_id = $1 AND event_seq = 2 AND role = 'consumed'",
    )
    .bind(&net_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(consumed_count, 1);

    let produced_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM causality_event_tokens \
         WHERE net_id = $1 AND event_seq = 2 AND role = 'produced'",
    )
    .bind(&net_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(produced_count, 1);
}

#[tokio::test]
async fn effect_completed_creates_cross_link() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&common::nats_url(), None)
        .await
        .expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;
    let _handle = spawn_causality_ingest(&nats, &db).await;

    let net_id = format!("test-{}", Uuid::new_v4().simple());
    let token_in = Uuid::new_v4().to_string();
    let token_out = Uuid::new_v4().to_string();
    let place_in = Uuid::new_v4().to_string();
    let place_out = Uuid::new_v4().to_string();
    let signal_key = format!("corr-{}", Uuid::new_v4().simple());

    let event = persisted_event(1, json!({
        "type": "EffectCompleted",
        "transition_id": Uuid::new_v4().to_string(),
        "transition_name": "dispatch",
        "consumed_tokens": [[&place_in, &token_in]],
        "produced_tokens": [[&place_out, token_json(&token_out)]],
        "effect_handler_id": "scheduler",
        "effect_result": { "signal_key": &signal_key }
    }));
    publish_event(nats.jetstream(), &net_id, "effect.completed", &event).await;
    wait_for_cross_link(&db, &signal_key, Duration::from_secs(5)).await;

    // Assert cross-link egress side
    let egress_net: String = sqlx::query_scalar(
        "SELECT egress_net FROM causality_cross_links WHERE signal_key = $1",
    )
    .bind(&signal_key)
    .fetch_one(&db)
    .await
    .expect("cross-link should exist");
    assert_eq!(egress_net, net_id);
}

#[tokio::test]
async fn bridge_transfer_links_cross_net() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&common::nats_url(), None)
        .await
        .expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;
    let _handle = spawn_causality_ingest(&nats, &db).await;

    let net_a = format!("test-a-{}", Uuid::new_v4().simple());
    let net_b = format!("test-b-{}", Uuid::new_v4().simple());
    let token_seed = Uuid::new_v4().to_string();
    let token_bridge = Uuid::new_v4().to_string();
    let token_dst = Uuid::new_v4().to_string();
    let signal_key = Uuid::new_v4().to_string();
    let place_start = Uuid::new_v4().to_string();
    let place_out = "bridge_outbox";
    let place_dst = "inbox";

    // 1. Seed token on net-A (creates process)
    let ev1 = persisted_event(1, token_created_event(&token_seed, &place_start));
    publish_event(nats.jetstream(), &net_a, "token.created", &ev1).await;
    wait_for_process_tag(&db, &token_seed, Duration::from_secs(5)).await;

    // 2. TransitionFired: consumes seed, produces bridge token (like the real engine)
    let ev2 = persisted_event(2, transition_fired_event(
        "send_to_other_net",
        &[(&place_start, &token_seed)],
        &[(&place_out, &token_bridge, json!({
            "id": &token_bridge,
            "color": { "type": "Unit" },
            "created_at": "2026-04-04T12:00:00Z",
            "created_by_event": 2
        }))],
    ));
    publish_event(nats.jetstream(), &net_a, "transition.fired", &ev2).await;
    wait_for_process_tag(&db, &token_bridge, Duration::from_secs(5)).await;

    // 3. TokenBridgedOut: the bridge token leaves net-A.
    //    created_by_event=2 points back to the transition at seq 2.
    let ev3 = persisted_event(3, json!({
        "type": "TokenBridgedOut",
        "token": {
            "id": &token_bridge,
            "color": { "type": "Unit" },
            "created_at": "2026-04-04T12:00:00Z",
            "created_by_event": 2
        },
        "source_place_id": place_out,
        "source_place_name": "Bridge Outbox",
        "target_net_id": &net_b,
        "target_place_name": place_dst,
        "transition_id": Uuid::new_v4().to_string(),
        "signal_key": &signal_key
    }));
    publish_event(nats.jetstream(), &net_a, "token.bridged_out", &ev3).await;
    wait_for_cross_link(&db, &signal_key, Duration::from_secs(5)).await;

    // 4. TokenCreated on net-B with signal_key (bridge arrival)
    let ev4 = persisted_event(1, token_created_with_signal_key(&token_dst, place_dst, &signal_key));
    publish_event(nats.jetstream(), &net_b, "token.created", &ev4).await;
    wait_for_process_tag(&db, &token_dst, Duration::from_secs(5)).await;

    // Assert: cross-link has both sides
    let (egress, ingress): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT egress_net, ingress_net FROM causality_cross_links WHERE signal_key = $1",
    )
    .bind(&signal_key)
    .fetch_one(&db)
    .await
    .expect("cross-link should exist");
    assert_eq!(egress.as_deref(), Some(net_a.as_str()));
    assert_eq!(ingress.as_deref(), Some(net_b.as_str()));

    // Assert: token on net-B inherited process tags from net-A
    let tag: Option<String> = sqlx::query_scalar(
        "SELECT process_id FROM causality_process_tags WHERE token_id = $1",
    )
    .bind(&token_dst)
    .fetch_optional(&db)
    .await
    .expect("query process tags");
    assert_eq!(
        tag.as_deref(),
        Some(token_seed.as_str()),
        "bridged token should inherit source process"
    );
}

#[tokio::test]
async fn duplicate_events_are_idempotent() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&common::nats_url(), None)
        .await
        .expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;
    let _handle = spawn_causality_ingest(&nats, &db).await;

    let net_id = format!("test-{}", Uuid::new_v4().simple());
    let token_id = Uuid::new_v4().to_string();
    let place_id = Uuid::new_v4().to_string();

    let event = persisted_event(1, token_created_event(&token_id, &place_id));

    // Publish same event twice
    publish_event(nats.jetstream(), &net_id, "token.created", &event).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    publish_event(nats.jetstream(), &net_id, "token.created", &event).await;

    wait_for_causality_event(&db, &net_id, 1, Duration::from_secs(5)).await;
    // Extra wait to ensure second message is also processed
    tokio::time::sleep(Duration::from_millis(500)).await;

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM causality_events WHERE net_id = $1 AND event_seq = 1",
    )
    .bind(&net_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(count, 1, "duplicate event should not create second row");
}

/// Signal-injected tokens (with signal_key) should inherit process tags
/// from the egress event that produced the signal_key, NOT create new processes.
#[tokio::test]
async fn signal_key_inherits_process_tags() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&common::nats_url(), None)
        .await
        .expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;
    let _handle = spawn_causality_ingest(&nats, &db).await;

    let net_id = format!("test-{}", Uuid::new_v4().simple());
    let token_seed = Uuid::new_v4().to_string();
    let token_submitted = Uuid::new_v4().to_string();
    let token_signal = Uuid::new_v4().to_string();
    let signal_key = format!("job-{}:0", Uuid::new_v4().simple());
    let place_start = Uuid::new_v4().to_string();
    let place_submitted = Uuid::new_v4().to_string();

    // 1. Seed token → creates process
    let ev1 = persisted_event(1, token_created_event(&token_seed, &place_start));
    publish_event(nats.jetstream(), &net_id, "token.created", &ev1).await;
    wait_for_process_tag(&db, &token_seed, Duration::from_secs(5)).await;

    // 2. EffectCompleted (executor_submit) → consumes seed, produces submitted, records signal_key
    let ev2 = persisted_event(2, json!({
        "type": "EffectCompleted",
        "transition_id": Uuid::new_v4().to_string(),
        "transition_name": "submit",
        "consumed_tokens": [[&place_start, &token_seed]],
        "produced_tokens": [[&place_submitted, token_json(&token_submitted)]],
        "effect_handler_id": "executor_submit",
        "effect_result": { "signal_key": &signal_key }
    }));
    publish_event(nats.jetstream(), &net_id, "effect.completed", &ev2).await;
    wait_for_cross_link(&db, &signal_key, Duration::from_secs(5)).await;

    // 3. Signal injection with signal_key → should inherit seed's process, NOT create new one
    let ev3 = persisted_event(3, token_created_with_signal_key(
        &token_signal, "sig_completed", &signal_key
    ));
    publish_event(nats.jetstream(), &net_id, "token.created", &ev3).await;
    wait_for_process_tag(&db, &token_signal, Duration::from_secs(5)).await;

    // Assert: signal token inherited process_id from seed token
    let signal_process: String = sqlx::query_scalar(
        "SELECT process_id FROM causality_process_tags WHERE token_id = $1",
    )
    .bind(&token_signal)
    .fetch_one(&db)
    .await
    .expect("signal token should have process tag");

    assert_eq!(
        signal_process, token_seed,
        "signal-injected token should inherit seed token's process"
    );

    // Assert: no extra processes were created (only 1 — the seed)
    let process_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT process_id)::bigint FROM causality_process_tags \
         WHERE token_id IN (SELECT token_id FROM causality_event_tokens WHERE net_id = $1)",
    )
    .bind(&net_id)
    .fetch_one(&db)
    .await
    .unwrap();

    assert_eq!(process_count, 1, "should have exactly 1 process, not more");
}

/// Tokens with created_by_event but no signal_key should NOT create processes
/// (they're produced by transitions and inherit via propagation).
#[tokio::test]
async fn non_seed_token_without_signal_key_does_not_create_process() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&common::nats_url(), None)
        .await
        .expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;
    let _handle = spawn_causality_ingest(&nats, &db).await;

    let net_id = format!("test-{}", Uuid::new_v4().simple());
    let token_id = Uuid::new_v4().to_string();

    // Token with created_by_event set (produced by a transition) but no signal_key
    let event = persisted_event(1, json!({
        "type": "TokenCreated",
        "token": {
            "id": &token_id,
            "color": { "type": "Unit" },
            "created_at": "2026-04-04T12:00:00Z",
            "created_by_event": 5
        },
        "place_id": "some_place",
        "place_name": "Some Place"
    }));
    publish_event(nats.jetstream(), &net_id, "token.created", &event).await;
    wait_for_causality_event(&db, &net_id, 1, Duration::from_secs(5)).await;
    // Small extra wait
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Should NOT have a process tag
    let has_tag: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM causality_process_tags WHERE token_id = $1)",
    )
    .bind(&token_id)
    .fetch_one(&db)
    .await
    .unwrap_or(false);

    assert!(!has_tag, "token with created_by_event should NOT self-tag as process");
}
