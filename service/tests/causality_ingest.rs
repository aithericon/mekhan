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
use serial_test::serial;

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
    // Post-multi-tenancy subject scheme: petri.{ws}.{net}.events.{suffix}
    // (the `events` category now follows the net; the ingest consumer filters
    // on `petri.*.*.events.>`). The ws token is cosmetic here — the projector
    // extracts net_id from subject index 2 and resolves the real workspace via
    // DB, so the nil/default workspace token just satisfies the filter.
    let ws = "00000000-0000-0000-0000-000000000000";
    let subject = format!("petri.{ws}.{net_id}.events.{event_suffix}");
    let bytes = serde_json::to_vec(payload).unwrap();
    js.publish(subject, bytes.into())
        .await
        .expect("publish event")
        .await
        .expect("event ACK");
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
            panic!("causality_cross_links row ({signal_key}) did not appear within {timeout:?}");
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
    let kv = nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("create KV");
    let sub_mgr = Arc::new(SubscriptionManager::new(kv, nats.jetstream().clone(), db.clone()));
    let live = LiveBroadcasts::new();
    let handle = tokio::spawn(async move {
        start_causality_ingest(
            nats,
            db,
            sub_mgr,
            live,
            None,
            "mekhan-artifacts".to_string(),
        )
        .await;
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

fn token_created_with_signal_key(
    token_id: &str,
    place_id: &str,
    signal_key: &str,
) -> serde_json::Value {
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
    consumed: &[(&str, &str)],                    // (place_id, token_id)
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
#[serial]
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
    let process_tag: String =
        sqlx::query_scalar("SELECT process_id FROM causality_process_tags WHERE token_id = $1")
            .bind(&token_id)
            .fetch_one(&db)
            .await
            .expect("fetch process tag");
    assert_eq!(process_tag, token_id, "seed token should self-tag");

    // Assert hpi_processes auto-created
    let proc_status: String =
        sqlx::query_scalar("SELECT status FROM hpi_processes WHERE process_id = $1")
            .bind(&token_id)
            .fetch_one(&db)
            .await
            .expect("fetch process status");
    assert_eq!(proc_status, "active");
}

#[tokio::test]
#[serial]
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
    let tag: String =
        sqlx::query_scalar("SELECT process_id FROM causality_process_tags WHERE token_id = $1")
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
#[serial]
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

    let event = persisted_event(
        1,
        json!({
            "type": "EffectCompleted",
            "transition_id": Uuid::new_v4().to_string(),
            "transition_name": "dispatch",
            "consumed_tokens": [[&place_in, &token_in]],
            "produced_tokens": [[&place_out, token_json(&token_out)]],
            "effect_handler_id": "scheduler",
            "effect_result": { "signal_key": &signal_key }
        }),
    );
    publish_event(nats.jetstream(), &net_id, "effect.completed", &event).await;
    wait_for_cross_link(&db, &signal_key, Duration::from_secs(5)).await;

    // Assert cross-link egress side
    let egress_net: String =
        sqlx::query_scalar("SELECT egress_net FROM causality_cross_links WHERE signal_key = $1")
            .bind(&signal_key)
            .fetch_one(&db)
            .await
            .expect("cross-link should exist");
    assert_eq!(egress_net, net_id);
}

#[tokio::test]
#[serial]
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
    let ev2 = persisted_event(
        2,
        transition_fired_event(
            "send_to_other_net",
            &[(&place_start, &token_seed)],
            &[(
                place_out,
                &token_bridge,
                json!({
                    "id": &token_bridge,
                    "color": { "type": "Unit" },
                    "created_at": "2026-04-04T12:00:00Z",
                    "created_by_event": 2
                }),
            )],
        ),
    );
    publish_event(nats.jetstream(), &net_a, "transition.fired", &ev2).await;
    wait_for_process_tag(&db, &token_bridge, Duration::from_secs(5)).await;

    // 3. TokenBridgedOut: the bridge token leaves net-A.
    //    created_by_event=2 points back to the transition at seq 2.
    let ev3 = persisted_event(
        3,
        json!({
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
        }),
    );
    publish_event(nats.jetstream(), &net_a, "token.bridged_out", &ev3).await;
    wait_for_cross_link(&db, &signal_key, Duration::from_secs(5)).await;

    // 4. TokenCreated on net-B with signal_key (bridge arrival)
    let ev4 = persisted_event(
        1,
        token_created_with_signal_key(&token_dst, place_dst, &signal_key),
    );
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
    let tag: Option<String> =
        sqlx::query_scalar("SELECT process_id FROM causality_process_tags WHERE token_id = $1")
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
#[serial]
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
#[serial]
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
    let ev2 = persisted_event(
        2,
        json!({
            "type": "EffectCompleted",
            "transition_id": Uuid::new_v4().to_string(),
            "transition_name": "submit",
            "consumed_tokens": [[&place_start, &token_seed]],
            "produced_tokens": [[&place_submitted, token_json(&token_submitted)]],
            "effect_handler_id": "executor_submit",
            "effect_result": { "signal_key": &signal_key }
        }),
    );
    publish_event(nats.jetstream(), &net_id, "effect.completed", &ev2).await;
    wait_for_cross_link(&db, &signal_key, Duration::from_secs(5)).await;

    // 3. Signal injection with signal_key → should inherit seed's process, NOT create new one
    let ev3 = persisted_event(
        3,
        token_created_with_signal_key(&token_signal, "sig_completed", &signal_key),
    );
    publish_event(nats.jetstream(), &net_id, "token.created", &ev3).await;
    wait_for_process_tag(&db, &token_signal, Duration::from_secs(5)).await;

    // Assert: signal token inherited process_id from seed token
    let signal_process: String =
        sqlx::query_scalar("SELECT process_id FROM causality_process_tags WHERE token_id = $1")
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
#[serial]
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
    let event = persisted_event(
        1,
        json!({
            "type": "TokenCreated",
            "token": {
                "id": &token_id,
                "color": { "type": "Unit" },
                "created_at": "2026-04-04T12:00:00Z",
                "created_by_event": 5
            },
            "place_id": "some_place",
            "place_name": "Some Place"
        }),
    );
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

    assert!(
        !has_tag,
        "token with created_by_event should NOT self-tag as process"
    );
}

// ── Pool-net process-tag containment ───────────────────────────────────────
//
// A shared `pool-*` net's capacity unit is consumed and re-produced on every
// lease cycle, so it accumulates the process tags of every instance that ever
// leased it. Pre-fix, a grant bridging back out smeared those foreign tags
// into the receiving instance net: catalogue artifacts landed on a sibling
// run's process, process_complete cross-fired, and process_start renamed
// every resolved process. These tests pin the containment rules.

/// Fetch the full tag set for a token.
async fn process_tags_for(db: &sqlx::PgPool, token_id: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT process_id FROM causality_process_tags WHERE token_id = $1 ORDER BY process_id",
    )
    .bind(token_id)
    .fetch_all(db)
    .await
    .expect("query process tags")
}

fn token_bridged_out_event(
    token_id: &str,
    source_place: &str,
    target_net: &str,
    target_place: &str,
    signal_key: &str,
) -> serde_json::Value {
    json!({
        "type": "TokenBridgedOut",
        "token": token_json(token_id),
        "source_place_id": source_place,
        "source_place_name": source_place,
        "target_net_id": target_net,
        "target_place_name": target_place,
        "transition_id": Uuid::new_v4().to_string(),
        "signal_key": signal_key
    })
}

/// Seed tokens in infrastructure nets (pool capacity units) must NOT
/// auto-create an HPI process or self-tag — they are plumbing, not process
/// roots. A phantom pool process would otherwise tag every grant derived
/// from the unit.
#[tokio::test]
#[serial]
async fn pool_seed_does_not_create_phantom_process() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&common::nats_url(), None)
        .await
        .expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;
    let _handle = spawn_causality_ingest(&nats, &db).await;

    let pool_net = format!("pool-{}", Uuid::new_v4());
    let unit_token = Uuid::new_v4().to_string();

    let ev = persisted_event(1, token_created_event(&unit_token, "free_units"));
    publish_event(nats.jetstream(), &pool_net, "token.created", &ev).await;
    wait_for_causality_event(&db, &pool_net, 1, Duration::from_secs(5)).await;
    // Tags are written after the event row — give the handler time to finish.
    tokio::time::sleep(Duration::from_millis(300)).await;

    assert!(
        process_tags_for(&db, &unit_token).await.is_empty(),
        "pool capacity seed must not self-tag as a process"
    );
    let has_process: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM hpi_processes WHERE process_id = $1)")
            .bind(&unit_token)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(
        !has_process,
        "pool capacity seed must not create a phantom hpi_processes row"
    );
}

/// The full two-instance lease cycle: instance A leases and releases the
/// pool's unit (the recycled unit legitimately accumulates A's tag inside the
/// pool), then instance B leases the same unit. The grant bridging back into
/// B's net must carry ONLY B's process tag — A's must be quarantined at the
/// pool boundary.
#[tokio::test]
#[serial]
async fn pool_grant_does_not_bleed_foreign_process_tags() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&common::nats_url(), None)
        .await
        .expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;
    let _handle = spawn_causality_ingest(&nats, &db).await;

    let net_a = format!("inst-a-{}", Uuid::new_v4().simple());
    let net_b = format!("inst-b-{}", Uuid::new_v4().simple());
    let pool_net = format!("pool-{}", Uuid::new_v4());

    let seed_a = Uuid::new_v4().to_string();
    let seed_b = Uuid::new_v4().to_string();
    let req_a_out = Uuid::new_v4().to_string();
    let req_a_in = Uuid::new_v4().to_string();
    let unit0 = Uuid::new_v4().to_string();
    let grant_a = Uuid::new_v4().to_string();
    let hold_a = Uuid::new_v4().to_string();
    let grant_a_in = Uuid::new_v4().to_string();
    let rel_a_out = Uuid::new_v4().to_string();
    let rel_a_in = Uuid::new_v4().to_string();
    let unit1 = Uuid::new_v4().to_string();
    let req_b_out = Uuid::new_v4().to_string();
    let req_b_in = Uuid::new_v4().to_string();
    let grant_b = Uuid::new_v4().to_string();
    let hold_b = Uuid::new_v4().to_string();
    let grant_b_in = Uuid::new_v4().to_string();

    let k_req_a = format!("claim-{}", Uuid::new_v4().simple());
    let k_grant_a = format!("grant-{}", Uuid::new_v4().simple());
    let k_rel_a = format!("release-{}", Uuid::new_v4().simple());
    let k_req_b = format!("claim-{}", Uuid::new_v4().simple());
    let k_grant_b = format!("grant-{}", Uuid::new_v4().simple());

    // Instance A: seed (process PA) → claim request bridges to the pool.
    let ev = persisted_event(1, token_created_event(&seed_a, "start"));
    publish_event(nats.jetstream(), &net_a, "token.created", &ev).await;
    wait_for_process_tag(&db, &seed_a, Duration::from_secs(5)).await;

    let ev = persisted_event(
        2,
        transition_fired_event(
            "t_claim",
            &[("start", &seed_a)],
            &[("claim_outbox", &req_a_out, token_json(&req_a_out))],
        ),
    );
    publish_event(nats.jetstream(), &net_a, "transition.fired", &ev).await;
    let ev = persisted_event(
        3,
        token_bridged_out_event(
            &req_a_out,
            "claim_outbox",
            &pool_net,
            "claim_inbox",
            &k_req_a,
        ),
    );
    publish_event(nats.jetstream(), &net_a, "token.bridged_out", &ev).await;
    wait_for_cross_link(&db, &k_req_a, Duration::from_secs(5)).await;

    // Pool: request arrives (inherits PA — instance-net egress is unfiltered),
    // capacity unit seeded (no tag), grant fired.
    let ev = persisted_event(
        1,
        token_created_with_signal_key(&req_a_in, "claim_inbox", &k_req_a),
    );
    publish_event(nats.jetstream(), &pool_net, "token.created", &ev).await;
    wait_for_process_tag(&db, &req_a_in, Duration::from_secs(5)).await;
    assert_eq!(
        process_tags_for(&db, &req_a_in).await,
        vec![seed_a.clone()],
        "claim request entering the pool should carry A's process tag"
    );

    let ev = persisted_event(2, token_created_event(&unit0, "free_units"));
    publish_event(nats.jetstream(), &pool_net, "token.created", &ev).await;
    wait_for_causality_event(&db, &pool_net, 2, Duration::from_secs(5)).await;

    let ev = persisted_event(
        3,
        transition_fired_event(
            "t_grant",
            &[("claim_inbox", &req_a_in), ("free_units", &unit0)],
            &[
                ("grant_outbox", &grant_a, token_json(&grant_a)),
                ("in_use", &hold_a, token_json(&hold_a)),
            ],
        ),
    );
    publish_event(nats.jetstream(), &pool_net, "transition.fired", &ev).await;
    wait_for_process_tag(&db, &grant_a, Duration::from_secs(5)).await;

    // Grant bridges back into A's own net: A's tag comes home (pool egress
    // filter must keep own-net tags).
    let ev = persisted_event(
        4,
        token_bridged_out_event(&grant_a, "grant_outbox", &net_a, "grant_inbox", &k_grant_a),
    );
    publish_event(nats.jetstream(), &pool_net, "token.bridged_out", &ev).await;
    wait_for_cross_link(&db, &k_grant_a, Duration::from_secs(5)).await;
    let ev = persisted_event(
        4,
        token_created_with_signal_key(&grant_a_in, "grant_inbox", &k_grant_a),
    );
    publish_event(nats.jetstream(), &net_a, "token.created", &ev).await;
    wait_for_process_tag(&db, &grant_a_in, Duration::from_secs(5)).await;
    assert_eq!(
        process_tags_for(&db, &grant_a_in).await,
        vec![seed_a.clone()],
        "grant returning to A's own net should carry A's process tag"
    );

    // A releases: the recycled unit (unit1) legitimately picks up A's tag
    // INSIDE the pool.
    let ev = persisted_event(
        5,
        transition_fired_event(
            "t_done",
            &[("grant_inbox", &grant_a_in)],
            &[("release_outbox", &rel_a_out, token_json(&rel_a_out))],
        ),
    );
    publish_event(nats.jetstream(), &net_a, "transition.fired", &ev).await;
    let ev = persisted_event(
        6,
        token_bridged_out_event(
            &rel_a_out,
            "release_outbox",
            &pool_net,
            "release_inbox",
            &k_rel_a,
        ),
    );
    publish_event(nats.jetstream(), &net_a, "token.bridged_out", &ev).await;
    wait_for_cross_link(&db, &k_rel_a, Duration::from_secs(5)).await;
    let ev = persisted_event(
        4,
        token_created_with_signal_key(&rel_a_in, "release_inbox", &k_rel_a),
    );
    publish_event(nats.jetstream(), &pool_net, "token.created", &ev).await;
    wait_for_process_tag(&db, &rel_a_in, Duration::from_secs(5)).await;

    let ev = persisted_event(
        5,
        transition_fired_event(
            "t_release",
            &[("in_use", &hold_a), ("release_inbox", &rel_a_in)],
            &[("free_units", &unit1, token_json(&unit1))],
        ),
    );
    publish_event(nats.jetstream(), &pool_net, "transition.fired", &ev).await;
    wait_for_process_tag(&db, &unit1, Duration::from_secs(5)).await;
    assert_eq!(
        process_tags_for(&db, &unit1).await,
        vec![seed_a.clone()],
        "recycled unit accumulates the prior holder's tag inside the pool"
    );

    // Instance B: seed (process PB) → claim → grant consumes the
    // contaminated unit1.
    let ev = persisted_event(1, token_created_event(&seed_b, "start"));
    publish_event(nats.jetstream(), &net_b, "token.created", &ev).await;
    wait_for_process_tag(&db, &seed_b, Duration::from_secs(5)).await;

    let ev = persisted_event(
        2,
        transition_fired_event(
            "t_claim",
            &[("start", &seed_b)],
            &[("claim_outbox", &req_b_out, token_json(&req_b_out))],
        ),
    );
    publish_event(nats.jetstream(), &net_b, "transition.fired", &ev).await;
    let ev = persisted_event(
        3,
        token_bridged_out_event(
            &req_b_out,
            "claim_outbox",
            &pool_net,
            "claim_inbox",
            &k_req_b,
        ),
    );
    publish_event(nats.jetstream(), &net_b, "token.bridged_out", &ev).await;
    wait_for_cross_link(&db, &k_req_b, Duration::from_secs(5)).await;
    let ev = persisted_event(
        6,
        token_created_with_signal_key(&req_b_in, "claim_inbox", &k_req_b),
    );
    publish_event(nats.jetstream(), &pool_net, "token.created", &ev).await;
    wait_for_process_tag(&db, &req_b_in, Duration::from_secs(5)).await;

    let ev = persisted_event(
        7,
        transition_fired_event(
            "t_grant",
            &[("claim_inbox", &req_b_in), ("free_units", &unit1)],
            &[
                ("grant_outbox", &grant_b, token_json(&grant_b)),
                ("in_use", &hold_b, token_json(&hold_b)),
            ],
        ),
    );
    publish_event(nats.jetstream(), &pool_net, "transition.fired", &ev).await;
    wait_for_process_tag(&db, &grant_b, Duration::from_secs(5)).await;
    // Inside the pool the grant carries BOTH tags (propagation from the
    // contaminated unit) — that is exactly what must not cross the bridge.
    let mut both = vec![seed_a.clone(), seed_b.clone()];
    both.sort();
    assert_eq!(
        process_tags_for(&db, &grant_b).await,
        both,
        "pool-internal grant carries both tags pre-bridge (the hazard)"
    );

    // The fix: the grant bridging into B's net carries ONLY B's process tag.
    let ev = persisted_event(
        8,
        token_bridged_out_event(&grant_b, "grant_outbox", &net_b, "grant_inbox", &k_grant_b),
    );
    publish_event(nats.jetstream(), &pool_net, "token.bridged_out", &ev).await;
    wait_for_cross_link(&db, &k_grant_b, Duration::from_secs(5)).await;
    let ev = persisted_event(
        4,
        token_created_with_signal_key(&grant_b_in, "grant_inbox", &k_grant_b),
    );
    publish_event(nats.jetstream(), &net_b, "token.created", &ev).await;
    wait_for_process_tag(&db, &grant_b_in, Duration::from_secs(5)).await;
    assert_eq!(
        process_tags_for(&db, &grant_b_in).await,
        vec![seed_b.clone()],
        "grant entering B's net must NOT carry A's process tag (pool bleed)"
    );
}

/// Even when a token DOES carry foreign tags (legacy contamination, or
/// in-net propagation that pre-dates the bridge filter), the projectors must
/// attribute to the firing net's own process: process_start renames and
/// process_complete completes ONLY the process homed on the event's net.
#[tokio::test]
#[serial]
async fn process_resolution_is_scoped_to_event_net() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&common::nats_url(), None)
        .await
        .expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;
    let _handle = spawn_causality_ingest(&nats, &db).await;

    let net_a = format!("inst-a-{}", Uuid::new_v4().simple());
    let net_b = format!("inst-b-{}", Uuid::new_v4().simple());
    let seed_a = Uuid::new_v4().to_string();
    let seed_b = Uuid::new_v4().to_string();

    let ev = persisted_event(1, token_created_event(&seed_a, "start"));
    publish_event(nats.jetstream(), &net_a, "token.created", &ev).await;
    wait_for_process_tag(&db, &seed_a, Duration::from_secs(5)).await;
    let ev = persisted_event(1, token_created_event(&seed_b, "start"));
    publish_event(nats.jetstream(), &net_b, "token.created", &ev).await;
    wait_for_process_tag(&db, &seed_b, Duration::from_secs(5)).await;

    // A token in net B contaminated with BOTH processes' tags.
    let mixed = Uuid::new_v4().to_string();
    for pid in [&seed_a, &seed_b] {
        sqlx::query("INSERT INTO causality_process_tags (token_id, process_id) VALUES ($1, $2)")
            .bind(&mixed)
            .bind(pid)
            .execute(&db)
            .await
            .unwrap();
    }

    // process_start fired in net B consuming the contaminated token: only
    // B's process may be renamed.
    let ev = persisted_event(
        2,
        json!({
            "type": "EffectCompleted",
            "transition_id": Uuid::new_v4().to_string(),
            "transition_name": "process_start",
            "consumed_tokens": [["p_start", &mixed]],
            "produced_tokens": [],
            "effect_handler_id": "process_start",
            "effect_result": { "name": "Renamed By B" }
        }),
    );
    publish_event(nats.jetstream(), &net_b, "effect.completed", &ev).await;
    wait_for_causality_event(&db, &net_b, 2, Duration::from_secs(5)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let name_b: Option<String> =
        sqlx::query_scalar("SELECT name FROM hpi_processes WHERE process_id = $1")
            .bind(&seed_b)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(name_b.as_deref(), Some("Renamed By B"));
    let name_a: Option<String> =
        sqlx::query_scalar("SELECT name FROM hpi_processes WHERE process_id = $1")
            .bind(&seed_a)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        name_a, None,
        "process_start in net B must not rename net A's process"
    );

    // process_complete fired in net B: only B's process completes.
    let ev = persisted_event(
        3,
        json!({
            "type": "EffectCompleted",
            "transition_id": Uuid::new_v4().to_string(),
            "transition_name": "process_complete",
            "consumed_tokens": [["p_end", &mixed]],
            "produced_tokens": [],
            "effect_handler_id": "process_complete",
            "effect_result": {}
        }),
    );
    publish_event(nats.jetstream(), &net_b, "effect.completed", &ev).await;
    wait_for_causality_event(&db, &net_b, 3, Duration::from_secs(5)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let status_b: String =
        sqlx::query_scalar("SELECT status FROM hpi_processes WHERE process_id = $1")
            .bind(&seed_b)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(status_b, "completed");
    let status_a: String =
        sqlx::query_scalar("SELECT status FROM hpi_processes WHERE process_id = $1")
            .bind(&seed_a)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        status_a, "active",
        "process_complete in net B must not cross-fire onto net A's process"
    );
}

/// Spawned sub-workflow child nets seed their own process — the process row
/// must be stamped with the CHILD net id (pre-fix it was NULL, breaking
/// net-scoped resolution for everything the child does).
#[tokio::test]
#[serial]
async fn seed_process_is_stamped_with_event_net() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&common::nats_url(), None)
        .await
        .expect("connect NATS");
    ensure_petri_global_stream(nats.jetstream()).await;
    let _handle = spawn_causality_ingest(&nats, &db).await;

    // A child net id is an arbitrary uuid (no mekhan- prefix, no
    // _instance_id in the seed's data).
    let child_net = Uuid::new_v4().to_string();
    let seed = Uuid::new_v4().to_string();

    let ev = persisted_event(1, token_created_event(&seed, "start"));
    publish_event(nats.jetstream(), &child_net, "token.created", &ev).await;
    wait_for_process_tag(&db, &seed, Duration::from_secs(5)).await;

    let net: Option<String> =
        sqlx::query_scalar("SELECT net_id FROM hpi_processes WHERE process_id = $1")
            .bind(&seed)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        net.as_deref(),
        Some(child_net.as_str()),
        "seed-created process must be homed on the event's net"
    );
}
