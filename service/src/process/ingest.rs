use futures::StreamExt;
use serde::Deserialize;
use sqlx::PgPool;

use crate::nats::MekhanNats;
use super::model::{LogCommand, MetricCommand, ProcessEventCommand, ProcessUpdateType};

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

/// Start the NATS process event ingest listener.
///
/// Subscribes to `process.events.>` on the `PROCESS` JetStream stream
/// and upserts each event into the `hpi_processes` Postgres table.
pub async fn start_process_event_ingest(nats: MekhanNats, db: PgPool) {
    if let Err(e) = nats.ensure_process_stream().await {
        tracing::error!("failed to create PROCESS stream: {e}");
        return;
    }

    let consumer = match nats.process_event_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create process event consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start process event message stream: {e}");
            return;
        }
    };

    tracing::info!("process event ingest started on process.events.>");

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("process event ingest message error: {e}");
                continue;
            }
        };

        let cmd: ProcessEventCommand = match serde_json::from_slice(&msg.payload) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("process event ingest: failed to deserialize command: {e}");
                let _ = msg.ack().await;
                continue;
            }
        };

        // Resolve process_id from available identifiers (in priority order):
        // 1. Explicit trace_id on the command
        // 2. trace_id from Started metadata
        // 3. hpi_process_id (fallback — always present)
        let process_id = cmd
            .trace_id
            .clone()
            .or_else(|| {
                if let ProcessUpdateType::Started { ref metadata } = cmd.update_type {
                    metadata.trace_id.clone()
                } else {
                    None
                }
            })
            .unwrap_or_else(|| cmd.hpi_process_id.clone());

        // Extract name and status from the update type
        let (status, name, config) = match &cmd.update_type {
            ProcessUpdateType::Started { metadata } => {
                let config = serde_json::json!({
                    "namespace": cmd.namespace,
                    "steps": metadata.steps,
                    "description": metadata.description,
                });
                ("active", metadata.name.clone(), config)
            }
            ProcessUpdateType::StepStarted { .. }
            | ProcessUpdateType::StepCompleted { .. }
            | ProcessUpdateType::Progress { .. }
            | ProcessUpdateType::ExecutionStarted { .. }
            | ProcessUpdateType::ExecutionProgress { .. }
            | ProcessUpdateType::ExecutionCompleted { .. }
            | ProcessUpdateType::ArtifactLogged { .. } => {
                ("active", None, serde_json::Value::Object(Default::default()))
            }
            ProcessUpdateType::Completed { .. } => {
                ("completed", None, serde_json::Value::Object(Default::default()))
            }
            ProcessUpdateType::Failed { .. }
            | ProcessUpdateType::StepFailed { .. }
            | ProcessUpdateType::ExecutionFailed { .. } => {
                ("failed", None, serde_json::Value::Object(Default::default()))
            }
        };

        let event_type_str = format!("{:?}", std::mem::discriminant(&cmd.update_type));

        let result = sqlx::query(
            r#"
            INSERT INTO hpi_processes (
                process_id, name, status, config, created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, NOW(), NOW()
            )
            ON CONFLICT (process_id) DO UPDATE SET
                status = $3,
                name = COALESCE(EXCLUDED.name, hpi_processes.name),
                config = CASE
                    WHEN EXCLUDED.config != '{}'::jsonb THEN EXCLUDED.config
                    ELSE hpi_processes.config
                END,
                updated_at = NOW()
            "#,
        )
        .bind(&process_id)
        .bind(&name)
        .bind(status)
        .bind(&config)
        .execute(&db)
        .await;

        match result {
            Ok(_) => {
                tracing::debug!(
                    process_id = %process_id,
                    event_type = %event_type_str,
                    "processed event",
                );
            }
            Err(e) => {
                tracing::error!(
                    process_id = %process_id,
                    "process event insert failed: {e}",
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

    tracing::warn!("process event ingest stream ended");
}

/// Start the NATS metric ingest listener.
///
/// Subscribes to `process.metrics.>` on the `PROCESS` JetStream stream
/// and inserts each metric into the `hpi_metrics` Postgres table.
pub async fn start_metric_ingest(nats: MekhanNats, db: PgPool) {
    if let Err(e) = nats.ensure_process_stream().await {
        tracing::error!("failed to create PROCESS stream: {e}");
        return;
    }

    let consumer = match nats.process_metric_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create metric consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start metric message stream: {e}");
            return;
        }
    };

    tracing::info!("metric ingest started on process.metrics.>");

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("metric ingest message error: {e}");
                continue;
            }
        };

        let cmd: MetricCommand = match serde_json::from_slice(&msg.payload) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("metric ingest: failed to deserialize command: {e}");
                let _ = msg.ack().await;
                continue;
            }
        };

        let timestamp = cmd
            .timestamp
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);

        // Auto-create process if unknown process_id
        let _ = sqlx::query(
            "INSERT INTO hpi_processes (process_id, status, config, created_at, updated_at) \
             VALUES ($1, 'active', '{}'::jsonb, NOW(), NOW()) \
             ON CONFLICT (process_id) DO NOTHING",
        )
        .bind(&cmd.process_id)
        .execute(&db)
        .await;

        let result = sqlx::query(
            "INSERT INTO hpi_metrics (process_id, key, value, timestamp) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(&cmd.process_id)
        .bind(&cmd.key)
        .bind(cmd.value)
        .bind(timestamp)
        .execute(&db)
        .await;

        match result {
            Ok(_) => {
                tracing::debug!(
                    process_id = %cmd.process_id,
                    key = %cmd.key,
                    "ingested metric",
                );
            }
            Err(e) => {
                tracing::error!(
                    process_id = %cmd.process_id,
                    key = %cmd.key,
                    "metric insert failed: {e}",
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

    tracing::warn!("metric ingest stream ended");
}

/// Start the NATS log ingest listener.
///
/// Subscribes to `process.logs.>` on the `PROCESS` JetStream stream
/// and inserts each log into the `hpi_logs` Postgres table.
pub async fn start_log_ingest(nats: MekhanNats, db: PgPool) {
    if let Err(e) = nats.ensure_process_stream().await {
        tracing::error!("failed to create PROCESS stream: {e}");
        return;
    }

    let consumer = match nats.process_log_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create log consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start log message stream: {e}");
            return;
        }
    };

    tracing::info!("log ingest started on process.logs.>");

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("log ingest message error: {e}");
                continue;
            }
        };

        let cmd: LogCommand = match serde_json::from_slice(&msg.payload) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("log ingest: failed to deserialize command: {e}");
                let _ = msg.ack().await;
                continue;
            }
        };

        let timestamp = cmd
            .timestamp
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);

        let level = cmd.level.as_deref().unwrap_or("info");
        let detail = cmd.detail.unwrap_or(serde_json::Value::Object(Default::default()));

        // Auto-create process if unknown process_id
        let _ = sqlx::query(
            "INSERT INTO hpi_processes (process_id, status, config, created_at, updated_at) \
             VALUES ($1, 'active', '{}'::jsonb, NOW(), NOW()) \
             ON CONFLICT (process_id) DO NOTHING",
        )
        .bind(&cmd.process_id)
        .execute(&db)
        .await;

        let result = sqlx::query(
            "INSERT INTO hpi_logs (process_id, level, source, message, detail, timestamp) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&cmd.process_id)
        .bind(level)
        .bind(&cmd.source)
        .bind(&cmd.message)
        .bind(&detail)
        .bind(timestamp)
        .execute(&db)
        .await;

        match result {
            Ok(_) => {
                tracing::debug!(
                    process_id = %cmd.process_id,
                    "ingested log",
                );
            }
            Err(e) => {
                tracing::error!(
                    process_id = %cmd.process_id,
                    "log insert failed: {e}",
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

    tracing::warn!("log ingest stream ended");
}
