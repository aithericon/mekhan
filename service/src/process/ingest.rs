use futures::StreamExt;
use serde::Deserialize;
use sqlx::PgPool;

use crate::nats::MekhanNats;

/// Minimal deserialization of a HumanTaskRequest from the engine.
#[derive(Debug, Deserialize)]
struct HumanTaskRequestMsg {
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    net_id: Option<String>,
    #[serde(default)]
    place: Option<String>,
    title: String,
    #[serde(default, alias = "process_id")]
    hpi_process_id: Option<String>,
    #[serde(default)]
    response_subject: Option<String>,
}

/// Start the NATS human task request ingest listener.
///
/// Subscribes to `human.request.>` on the `HUMAN_REQUESTS` JetStream stream
/// and inserts each task into the `hpi_tasks` Postgres table.
pub async fn start_task_ingest(nats: MekhanNats, db: PgPool) {
    if let Err(e) = nats.ensure_human_stream().await {
        tracing::error!("failed to ensure HUMAN_REQUESTS stream: {e}");
        return;
    }

    let consumer = match nats.human_task_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create human task consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start human task message stream: {e}");
            return;
        }
    };

    tracing::info!("task ingest started on human.request.>");

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("task ingest message error: {e}");
                continue;
            }
        };

        // Keep the full payload as detail JSON
        let raw_value: serde_json::Value = match serde_json::from_slice(&msg.payload) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("task ingest: failed to parse payload as JSON: {e}");
                let _ = msg.ack().await;
                continue;
            }
        };

        let req: HumanTaskRequestMsg = match serde_json::from_value(raw_value.clone()) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("task ingest: failed to deserialize HumanTaskRequest: {e}");
                let _ = msg.ack().await;
                continue;
            }
        };

        // Extract net_id and place from subject if not in payload
        let subject = msg.subject.as_str();
        let (subj_net_id, subj_place) = parse_human_request_subject(subject);
        let net_id = req.net_id.as_deref().or(subj_net_id);
        let place = req.place.as_deref().or(subj_place);

        let task_id = req
            .task_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let process_id = req.hpi_process_id.clone().unwrap_or_else(|| {
            // Fall back to net_id as process correlation
            net_id.unwrap_or("unknown").to_string()
        });

        // Build detail JSONB: original payload + routing metadata
        let mut detail = match raw_value {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
        if let Some(nid) = net_id {
            detail.insert("net_id".into(), serde_json::Value::String(nid.to_string()));
        }
        if let Some(p) = place {
            detail.insert("place".into(), serde_json::Value::String(p.to_string()));
        }
        if let Some(ref rs) = req.response_subject {
            detail.insert(
                "response_subject".into(),
                serde_json::Value::String(rs.clone()),
            );
        }
        let detail_value = serde_json::Value::Object(detail);

        // Auto-create process if not present
        let _ = sqlx::query(
            "INSERT INTO hpi_processes (process_id, status, config, created_at, updated_at) \
             VALUES ($1, 'active', '{}'::jsonb, NOW(), NOW()) \
             ON CONFLICT (process_id) DO NOTHING",
        )
        .bind(&process_id)
        .execute(&db)
        .await;

        let result = sqlx::query(
            "INSERT INTO hpi_tasks (id, process_id, title, status, detail, created_at) \
             VALUES ($1, $2, $3, 'pending', $4, NOW()) \
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(&task_id)
        .bind(&process_id)
        .bind(&req.title)
        .bind(&detail_value)
        .execute(&db)
        .await;

        match result {
            Ok(_) => {
                tracing::info!(
                    task_id = %task_id,
                    process_id = %process_id,
                    title = %req.title,
                    "ingested human task",
                );
            }
            Err(e) => {
                tracing::error!(
                    task_id = %task_id,
                    "task insert failed: {e}",
                );
                let _ = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        std::time::Duration::from_secs(2),
                    )))
                    .await;
                continue;
            }
        }

        let _ = msg.ack().await;
    }

    tracing::warn!("task ingest stream ended");
}

/// Parse `human.request.{net_id}.{place}` subject.
fn parse_human_request_subject(subject: &str) -> (Option<&str>, Option<&str>) {
    let rest = match subject.strip_prefix("human.request.") {
        Some(r) => r,
        None => return (None, None),
    };
    let mut parts = rest.splitn(2, '.');
    let net_id = parts.next();
    let place = parts.next();
    (net_id, place)
}

