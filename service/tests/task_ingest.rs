//! Integration tests for the human task ingest consumer.
//!
//! Validates that human task requests from NATS are correctly projected
//! into hpi_tasks with proper process auto-creation.
//!
//! Requires: `just -f aithericon-test-infra/justfile up` (Postgres + NATS)

mod common;

use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::stream::Config as StreamConfig;
use serde_json::json;
use uuid::Uuid;

use mekhan_service::nats::MekhanNats;
use mekhan_service::process::ingest::start_task_ingest;

// ── Helpers ────────────────────────────────────────────────────────────────

async fn ensure_human_stream(js: &jetstream::Context) {
    js.get_or_create_stream(StreamConfig {
        name: "HUMAN_REQUESTS".to_string(),
        subjects: vec!["human.request.>".to_string()],
        max_age: Duration::from_secs(300),
        ..Default::default()
    })
    .await
    .expect("create HUMAN_REQUESTS stream");
}

async fn publish_human_request(
    js: &jetstream::Context,
    net_id: &str,
    place: &str,
    payload: &serde_json::Value,
) {
    let subject = format!("human.request.{net_id}.{place}");
    let bytes = serde_json::to_vec(payload).unwrap();
    js.publish(subject, bytes.into())
        .await
        .expect("publish human request")
        .await
        .expect("human request ACK");
}

async fn wait_for_task(db: &sqlx::PgPool, task_id: &str, timeout: Duration) {
    let start = std::time::Instant::now();
    loop {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM hpi_tasks WHERE id = $1)")
                .bind(task_id)
                .fetch_one(db)
                .await
                .unwrap_or(false);

        if exists {
            return;
        }
        if start.elapsed() > timeout {
            panic!("hpi_tasks row ({task_id}) did not appear within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct IngestHandle(tokio::task::AbortHandle);
impl Drop for IngestHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn spawn_task_ingest(nats: &MekhanNats, db: &sqlx::PgPool) -> IngestHandle {
    if let Ok(stream) = nats.jetstream().get_stream("HUMAN_REQUESTS").await {
        let _ = stream.delete_consumer("mekhan-human-task-ingest").await;
        let _ = stream.purge().await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let nats = nats.clone();
    let db = db.clone();
    let handle = tokio::spawn(async move {
        start_task_ingest(nats, db).await;
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    IngestHandle(handle.abort_handle())
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn human_request_creates_task() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&aithericon_test_infra::nats_url())
        .await
        .expect("connect NATS");
    ensure_human_stream(nats.jetstream()).await;
    let _handle = spawn_task_ingest(&nats, &db).await;

    let net_id = format!("test-{}", Uuid::new_v4().simple());
    let task_id = Uuid::new_v4().to_string();
    let process_id = format!("proc-{}", Uuid::new_v4().simple());

    let payload = json!({
        "task_id": &task_id,
        "title": "Review experiment results",
        "hpi_process_id": &process_id,
        "instructions_mdsvex": "Please review the GP model fit."
    });
    publish_human_request(nats.jetstream(), &net_id, "review", &payload).await;

    wait_for_task(&db, &task_id, Duration::from_secs(5)).await;

    // Assert task fields
    let (title, status, pid): (String, String, String) = sqlx::query_as(
        "SELECT title, status, process_id FROM hpi_tasks WHERE id = $1",
    )
    .bind(&task_id)
    .fetch_one(&db)
    .await
    .expect("fetch task");
    assert_eq!(title, "Review experiment results");
    assert_eq!(status, "pending");
    assert_eq!(pid, process_id);

    // Assert detail contains net_id and place
    let detail: serde_json::Value =
        sqlx::query_scalar("SELECT detail FROM hpi_tasks WHERE id = $1")
            .bind(&task_id)
            .fetch_one(&db)
            .await
            .expect("fetch detail");
    assert_eq!(detail["net_id"].as_str(), Some(net_id.as_str()));
    assert_eq!(detail["place"].as_str(), Some("review"));

    // Assert process auto-created
    let proc_status: String =
        sqlx::query_scalar("SELECT status FROM hpi_processes WHERE process_id = $1")
            .bind(&process_id)
            .fetch_one(&db)
            .await
            .expect("process should be auto-created");
    assert_eq!(proc_status, "active");
}

#[tokio::test]
async fn duplicate_task_is_idempotent() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&aithericon_test_infra::nats_url())
        .await
        .expect("connect NATS");
    ensure_human_stream(nats.jetstream()).await;
    let _handle = spawn_task_ingest(&nats, &db).await;

    let net_id = format!("test-{}", Uuid::new_v4().simple());
    let task_id = Uuid::new_v4().to_string();

    let payload = json!({
        "task_id": &task_id,
        "title": "Duplicate task",
        "hpi_process_id": "some-process"
    });

    // Publish same task twice
    publish_human_request(nats.jetstream(), &net_id, "review", &payload).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    publish_human_request(nats.jetstream(), &net_id, "review", &payload).await;

    wait_for_task(&db, &task_id, Duration::from_secs(5)).await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM hpi_tasks WHERE id = $1",
    )
    .bind(&task_id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(count, 1, "duplicate task should not create second row");
}

#[tokio::test]
async fn task_without_process_id_uses_net_id() {
    let db = common::create_test_db().await;
    let nats = MekhanNats::connect(&aithericon_test_infra::nats_url())
        .await
        .expect("connect NATS");
    ensure_human_stream(nats.jetstream()).await;
    let _handle = spawn_task_ingest(&nats, &db).await;

    let net_id = format!("test-{}", Uuid::new_v4().simple());
    let task_id = Uuid::new_v4().to_string();

    // No hpi_process_id in payload
    let payload = json!({
        "task_id": &task_id,
        "title": "Orphan task"
    });
    publish_human_request(nats.jetstream(), &net_id, "approve", &payload).await;

    wait_for_task(&db, &task_id, Duration::from_secs(5)).await;

    // process_id should fall back to net_id
    let pid: String = sqlx::query_scalar(
        "SELECT process_id FROM hpi_tasks WHERE id = $1",
    )
    .bind(&task_id)
    .fetch_one(&db)
    .await
    .expect("fetch process_id");
    assert_eq!(pid, net_id, "should fall back to net_id as process_id");
}
