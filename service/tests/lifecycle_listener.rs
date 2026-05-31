//! Tests for the NATS lifecycle listener.
//!
//! Validates that Mekhan correctly processes NetCompleted and NetCancelled
//! events from NATS and updates instance status in the database.
//!
//! Requires: `just -f aithericon-test-infra/justfile up` (Postgres + NATS)
//! Does NOT require a running petri-lab engine.

mod common;

use std::time::Duration;
use uuid::Uuid;

use async_nats::jetstream;
use async_nats::jetstream::stream::Config as StreamConfig;

use std::sync::Arc;

use mekhan_service::catalogue::subscriptions::SubscriptionManager;
use mekhan_service::lifecycle::start_lifecycle_listener;
use mekhan_service::nats::MekhanNats;

/// Create a SubscriptionManager for tests (required by start_lifecycle_listener).
async fn test_subscription_manager(nats: &MekhanNats) -> Arc<SubscriptionManager> {
    let kv = nats
        .ensure_catalogue_subscriptions_kv()
        .await
        .expect("create test KV bucket");
    Arc::new(SubscriptionManager::new(kv, nats.jetstream().clone()))
}

/// Ensure PETRI_GLOBAL stream exists on the test NATS instance.
/// The lifecycle consumer requires this stream.
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

/// Helper: insert a fake running instance directly into DB.
/// Creates a parent template first to satisfy foreign key constraints.
async fn insert_running_instance(db: &sqlx::PgPool, instance_id: Uuid, net_id: &str) {
    let template_id = Uuid::new_v4();
    let author_id = Uuid::new_v4();

    // Insert a minimal template (FK target)
    sqlx::query(
        r#"INSERT INTO workflow_templates
           (id, name, description, version, is_latest, published, graph, author_id)
           VALUES ($1, 'test-template', '', 1, true, true, '{}', $2)"#,
    )
    .bind(template_id)
    .bind(author_id)
    .execute(db)
    .await
    .expect("insert template");

    // Insert the running instance
    sqlx::query(
        r#"INSERT INTO workflow_instances
           (id, template_id, template_version, net_id, status, created_by, started_at, metadata)
           VALUES ($1, $2, 1, $3, 'running', $4, NOW(), '{}')"#,
    )
    .bind(instance_id)
    .bind(template_id)
    .bind(net_id)
    .bind(author_id)
    .execute(db)
    .await
    .expect("insert running instance");
}

/// Helper: fetch instance status from DB.
async fn get_instance_status(db: &sqlx::PgPool, instance_id: Uuid) -> String {
    sqlx::query_scalar::<_, String>("SELECT status FROM workflow_instances WHERE id = $1")
        .bind(instance_id)
        .fetch_one(db)
        .await
        .expect("fetch instance status")
}

/// Helper: publish a fake NetCompleted/NetCancelled event to NATS.
async fn publish_lifecycle_event(js: &jetstream::Context, net_id: &str, event_type: &str) {
    let subject = format!("petri.events.{net_id}.net.{event_type}");
    // The lifecycle listener only parses the subject, not the payload.
    // Send a minimal valid JSON payload.
    let payload = serde_json::json!({
        "sequence": 99,
        "timestamp": "2026-01-01T00:00:00Z",
        "event": {
            "type": event_type,
            "net_id": net_id
        },
        "hash": "fake",
        "previous_hash": null
    });
    let bytes = serde_json::to_vec(&payload).unwrap();

    js.publish(subject, bytes.into())
        .await
        .expect("publish lifecycle event")
        .await
        .expect("lifecycle event ACK");
}

/// Wait for an instance to reach a target status, with timeout.
async fn wait_for_status(db: &sqlx::PgPool, instance_id: Uuid, target: &str, timeout: Duration) {
    let start = std::time::Instant::now();
    loop {
        let status = get_instance_status(db, instance_id).await;
        if status == target {
            return;
        }
        if start.elapsed() > timeout {
            panic!(
                "instance {} did not reach status '{}' within {:?} (current: '{}')",
                instance_id, target, timeout, status
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn net_completed_updates_instance_status() {
    let db = common::create_test_db().await;
    let nats_url = common::nats_url();
    let nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("connect to NATS");

    // Ensure stream exists
    ensure_petri_global_stream(nats.jetstream()).await;

    // Start lifecycle listener in background
    let sub_mgr = test_subscription_manager(&nats).await;
    let listener_nats = nats.clone();
    let listener_db = db.clone();
    tokio::spawn(async move {
        start_lifecycle_listener(
            listener_nats,
            listener_db,
            sub_mgr,
            None,
            mekhan_service::triggers::ResultWaiters::new(),
        )
        .await;
    });

    // Give the listener a moment to subscribe
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Insert a fake running instance
    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{}", Uuid::new_v4().simple());
    insert_running_instance(&db, instance_id, &net_id).await;

    // Publish completion event
    publish_lifecycle_event(nats.jetstream(), &net_id, "completed").await;

    // Wait for the listener to process it
    wait_for_status(&db, instance_id, "completed", Duration::from_secs(5)).await;

    // Verify completed_at is set
    let completed_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT completed_at FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&db)
            .await
            .expect("fetch completed_at");
    assert!(completed_at.is_some(), "completed_at should be set");
}

#[tokio::test]
async fn net_cancelled_updates_instance_status() {
    let db = common::create_test_db().await;
    let nats_url = common::nats_url();
    let nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("connect to NATS");

    ensure_petri_global_stream(nats.jetstream()).await;

    let sub_mgr = test_subscription_manager(&nats).await;
    let listener_nats = nats.clone();
    let listener_db = db.clone();
    tokio::spawn(async move {
        start_lifecycle_listener(
            listener_nats,
            listener_db,
            sub_mgr,
            None,
            mekhan_service::triggers::ResultWaiters::new(),
        )
        .await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{}", Uuid::new_v4().simple());
    insert_running_instance(&db, instance_id, &net_id).await;

    publish_lifecycle_event(nats.jetstream(), &net_id, "cancelled").await;

    wait_for_status(&db, instance_id, "cancelled", Duration::from_secs(5)).await;
}

#[tokio::test]
async fn completed_event_is_idempotent() {
    let db = common::create_test_db().await;
    let nats_url = common::nats_url();
    let nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("connect to NATS");

    ensure_petri_global_stream(nats.jetstream()).await;

    let sub_mgr = test_subscription_manager(&nats).await;
    let listener_nats = nats.clone();
    let listener_db = db.clone();
    tokio::spawn(async move {
        start_lifecycle_listener(
            listener_nats,
            listener_db,
            sub_mgr,
            None,
            mekhan_service::triggers::ResultWaiters::new(),
        )
        .await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{}", Uuid::new_v4().simple());
    insert_running_instance(&db, instance_id, &net_id).await;

    // Publish twice
    publish_lifecycle_event(nats.jetstream(), &net_id, "completed").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    publish_lifecycle_event(nats.jetstream(), &net_id, "completed").await;

    wait_for_status(&db, instance_id, "completed", Duration::from_secs(5)).await;

    // Status should still be "completed" (not errored)
    let status = get_instance_status(&db, instance_id).await;
    assert_eq!(status, "completed");
}

#[tokio::test]
async fn already_completed_instance_ignores_cancel() {
    let db = common::create_test_db().await;
    let nats_url = common::nats_url();
    let nats = MekhanNats::connect(&nats_url, None)
        .await
        .expect("connect to NATS");

    ensure_petri_global_stream(nats.jetstream()).await;

    let sub_mgr = test_subscription_manager(&nats).await;
    let listener_nats = nats.clone();
    let listener_db = db.clone();
    tokio::spawn(async move {
        start_lifecycle_listener(
            listener_nats,
            listener_db,
            sub_mgr,
            None,
            mekhan_service::triggers::ResultWaiters::new(),
        )
        .await;
    });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{}", Uuid::new_v4().simple());
    insert_running_instance(&db, instance_id, &net_id).await;

    // Complete first
    publish_lifecycle_event(nats.jetstream(), &net_id, "completed").await;
    wait_for_status(&db, instance_id, "completed", Duration::from_secs(5)).await;

    // Then try to cancel — should be ignored (WHERE status = 'running' won't match)
    publish_lifecycle_event(nats.jetstream(), &net_id, "cancelled").await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let status = get_instance_status(&db, instance_id).await;
    assert_eq!(
        status, "completed",
        "completed instance should not be cancelled"
    );
}
