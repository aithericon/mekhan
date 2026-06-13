//! End-to-end coverage for the allocations projection ingest.
//!
//! Mirrors the harness of `step_executions_e2e.rs` (per-test consumer prefix
//! so the durable doesn't collide with the production `mekhan-allocations-v2`
//! owned by the live dev daemon), but drives the projection directly:
//! publishes synthetic engine events on a unique `pool-<uuid>` net into
//! PETRI_GLOBAL — a `resource_lease_acquire` `EffectCompleted`, then a
//! `resource_lease_release` — and polls the `allocations` table until the
//! grant's row materializes `held` → `released`.
//!
//! Requires the `just dev up` stack (NATS broker with the PETRI_GLOBAL
//! stream; the stream is race-created if absent so the test also runs against
//! the bare test infra). Run serially:
//! `cargo test --test allocations_ingest_e2e -- --test-threads=1`.

mod common;

use std::time::Duration;

use chrono::Utc;
use uuid::Uuid;

use petri_domain::{DomainEvent, PersistedEvent, PlaceId, Token, TokenColor, TransitionId};

use mekhan_service::nats::subjects::Subjects;
use mekhan_service::nats::MekhanNats;
use mekhan_service::projections::allocations::start_allocations_ingest;

fn effect_completed(
    seq: u64,
    handler: &str,
    effect_result: serde_json::Value,
    produced: Vec<(PlaceId, Token)>,
) -> PersistedEvent {
    PersistedEvent {
        sequence: seq,
        timestamp: Utc::now(),
        event: DomainEvent::EffectCompleted {
            transition_id: TransitionId("t_lease".into()),
            transition_name: None,
            consumed_tokens: vec![],
            produced_tokens: produced,
            effect_handler_id: handler.to_string(),
            effect_result,
            read_tokens: vec![],
            process_step_started: None,
            process_step_completed: None,
        },
        hash: String::new(),
        previous_hash: None,
    }
}

/// Publish one persisted event on the net's event subject, exactly where the
/// engine would put it (`petri.{ws}.{net_id}.events.effect.completed`).
async fn publish_event(nats: &MekhanNats, net_id: &str, ev: &PersistedEvent) {
    let subject = Subjects::for_event(&ev.event, Subjects::DEFAULT_WORKSPACE, Some(net_id));
    let payload = serde_json::to_vec(ev).expect("serialize event");
    nats.jetstream()
        .publish(subject, payload.into())
        .await
        .expect("publish")
        .await
        .expect("publish ack");
}

/// Ensure PETRI_GLOBAL exists (engine-owned on a live stack; race-created
/// here so the test also runs against the bare test-infra NATS).
async fn ensure_petri_global(nats: &MekhanNats) {
    if nats
        .jetstream()
        .get_stream(Subjects::STREAM_GLOBAL)
        .await
        .is_ok()
    {
        return;
    }
    let _ = nats
        .jetstream()
        .create_stream(async_nats::jetstream::stream::Config {
            name: Subjects::STREAM_GLOBAL.to_string(),
            subjects: vec!["petri.>".to_string()],
            ..Default::default()
        })
        .await;
}

#[tokio::test]
async fn allocations_materialize_from_synthetic_pool_events() {
    let nats_url = std::env::var("ENGINE_NATS_URL").unwrap_or_else(|_| common::nats_url());
    let db = common::create_test_db().await;

    let publisher = MekhanNats::connect(&nats_url, None).await.expect("nats");
    ensure_petri_global(&publisher).await;

    // Allocations consumer — needs a prefixed `MekhanNats` so its durable
    // doesn't collide with the production `mekhan-allocations-v2` durable
    // owned by the live dev daemon (test-prefixed durables start at
    // DeliverPolicy::New).
    let test_prefix = format!("test_alloc_{}", Uuid::new_v4().simple());
    let alloc_nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("nats")
        .with_consumer_prefix(test_prefix);
    {
        let db = db.clone();
        tokio::spawn(async move {
            start_allocations_ingest(alloc_nats, db).await;
        });
    }
    // Give the consumer a beat to come up before we start publishing events.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let resource_id = Uuid::new_v4();
    let net_id = format!("pool-{resource_id}");
    let grant_id = format!("mekhan-{}:lease1", Uuid::new_v4().simple());

    // ── Acquire: opens the row at `held` ────────────────────────────────────
    let lease = serde_json::json!({
        "grant_id": grant_id,
        "alloc_id": "job-e2e-42",
        "node": "gpu-node-3",
        "executor_namespace": "lease-e2e",
        "expiry": "2099-01-01T00:00:00Z",
        "scheduler": { "slurm": { "partition": "gpu" } },
    });
    let acquire = effect_completed(
        1,
        "resource_lease_acquire",
        serde_json::json!({ "alloc_id": "job-e2e-42", "lease": lease }),
        vec![],
    );
    publish_event(&publisher, &net_id, &acquire).await;

    // The durable starts at DeliverPolicy::New, so re-publish the (idempotent)
    // acquire while polling in case the first publish raced consumer creation
    // — the projection bootstraps from the FULL per-net history either way.
    let deadline = Duration::from_secs(30);
    let started = std::time::Instant::now();
    loop {
        let row: Option<(String, String)> = sqlx::query_as(
            "SELECT status, kind FROM allocations WHERE net_id = $1 AND grant_id = $2",
        )
        .bind(&net_id)
        .bind(&grant_id)
        .fetch_optional(&db)
        .await
        .unwrap();
        if let Some((status, kind)) = row {
            assert_eq!(kind, "datacenter_lease");
            assert_eq!(status, "held", "acquire should open the row at held");
            break;
        }
        if started.elapsed() > deadline {
            panic!("allocations row did not materialize within {deadline:?}");
        }
        publish_event(&publisher, &net_id, &acquire).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    // ── Release: moves the row to `released` ────────────────────────────────
    let release = effect_completed(
        2,
        "resource_lease_release",
        serde_json::json!({ "alloc_id": "job-e2e-42", "released": true }),
        vec![(
            PlaceId("p_released".into()),
            Token::new(TokenColor::Data(
                serde_json::json!({ "grant_id": grant_id }),
            )),
        )],
    );
    publish_event(&publisher, &net_id, &release).await;

    let deadline = Duration::from_secs(20);
    let started = std::time::Instant::now();
    loop {
        let status: String = sqlx::query_scalar(
            "SELECT status FROM allocations WHERE net_id = $1 AND grant_id = $2",
        )
        .bind(&net_id)
        .bind(&grant_id)
        .fetch_one(&db)
        .await
        .unwrap();
        if status == "released" {
            break;
        }
        if started.elapsed() > deadline {
            panic!("allocation did not release within {deadline:?} (status: {status})");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // ── Field assertions on the final row ───────────────────────────────────
    let (alloc_id, node, cluster_resource_id, node_id, last_sequence): (
        Option<String>,
        Option<String>,
        Option<Uuid>,
        Option<String>,
        i64,
    ) = sqlx::query_as(
        "SELECT alloc_id, node, cluster_resource_id, node_id, last_sequence \
         FROM allocations WHERE net_id = $1 AND grant_id = $2",
    )
    .bind(&net_id)
    .bind(&grant_id)
    .fetch_one(&db)
    .await
    .unwrap();

    assert_eq!(alloc_id.as_deref(), Some("job-e2e-42"));
    assert_eq!(node.as_deref(), Some("gpu-node-3"));
    assert_eq!(cluster_resource_id, Some(resource_id));
    assert_eq!(node_id.as_deref(), Some("lease1"));
    assert_eq!(last_sequence, 2);
}
