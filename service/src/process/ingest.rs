use futures::StreamExt;
use sqlx::PgPool;

use crate::nats::MekhanNats;
use super::model::{LogCommand, MetricCommand, ProcessEventCommand, ProcessUpdateType};

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

        // Use trace_id from the command, or from metadata for Started events
        let trace_id = cmd.trace_id.clone().or_else(|| {
            if let ProcessUpdateType::Started { ref metadata } = cmd.update_type {
                metadata.trace_id.clone()
            } else {
                None
            }
        });

        let trace_id = match trace_id {
            Some(t) => t,
            None => {
                tracing::warn!(
                    hpi_process_id = %cmd.hpi_process_id,
                    "process event has no trace_id, skipping",
                );
                let _ = msg.ack().await;
                continue;
            }
        };

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
                trace_id, name, status, hpi_process_id, config, created_at, updated_at
            ) VALUES (
                $1, $2, $3, $4, $5, NOW(), NOW()
            )
            ON CONFLICT (trace_id) DO UPDATE SET
                status = $3,
                name = COALESCE(EXCLUDED.name, hpi_processes.name),
                hpi_process_id = COALESCE(EXCLUDED.hpi_process_id, hpi_processes.hpi_process_id),
                config = CASE
                    WHEN EXCLUDED.config != '{}'::jsonb THEN EXCLUDED.config
                    ELSE hpi_processes.config
                END,
                updated_at = NOW()
            "#,
        )
        .bind(&trace_id)
        .bind(&name)
        .bind(status)
        .bind(&cmd.hpi_process_id)
        .bind(&config)
        .execute(&db)
        .await;

        match result {
            Ok(_) => {
                tracing::debug!(
                    trace_id = %trace_id,
                    event_type = %event_type_str,
                    "processed event",
                );
            }
            Err(e) => {
                tracing::error!(
                    trace_id = %trace_id,
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

        // Auto-create process if unknown trace_id
        let _ = sqlx::query(
            "INSERT INTO hpi_processes (trace_id, status, config, created_at, updated_at) \
             VALUES ($1, 'active', '{}'::jsonb, NOW(), NOW()) \
             ON CONFLICT (trace_id) DO NOTHING",
        )
        .bind(&cmd.trace_id)
        .execute(&db)
        .await;

        let result = sqlx::query(
            "INSERT INTO hpi_metrics (trace_id, span_id, key, value, timestamp) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&cmd.trace_id)
        .bind(&cmd.span_id)
        .bind(&cmd.key)
        .bind(cmd.value)
        .bind(timestamp)
        .execute(&db)
        .await;

        match result {
            Ok(_) => {
                tracing::debug!(
                    trace_id = %cmd.trace_id,
                    key = %cmd.key,
                    "ingested metric",
                );
            }
            Err(e) => {
                tracing::error!(
                    trace_id = %cmd.trace_id,
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

        // Auto-create process if unknown trace_id
        let _ = sqlx::query(
            "INSERT INTO hpi_processes (trace_id, status, config, created_at, updated_at) \
             VALUES ($1, 'active', '{}'::jsonb, NOW(), NOW()) \
             ON CONFLICT (trace_id) DO NOTHING",
        )
        .bind(&cmd.trace_id)
        .execute(&db)
        .await;

        let result = sqlx::query(
            "INSERT INTO hpi_logs (trace_id, span_id, level, source, message, detail, timestamp) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&cmd.trace_id)
        .bind(&cmd.span_id)
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
                    trace_id = %cmd.trace_id,
                    "ingested log",
                );
            }
            Err(e) => {
                tracing::error!(
                    trace_id = %cmd.trace_id,
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
