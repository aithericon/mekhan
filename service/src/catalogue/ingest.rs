use std::sync::Arc;

use futures::StreamExt;
use sqlx::PgPool;

use crate::nats::MekhanNats;
use super::model::CatalogueRegisterCommand;
use super::subscriptions::{SubscriptionManager, command_to_entry};

/// Start the NATS catalogue ingest listener.
///
/// Subscribes to `catalogue.commands.register` on the `CATALOGUE` JetStream
/// stream and inserts each command into the `catalogue_entries` Postgres table.
pub async fn start_catalogue_ingest(
    nats: MekhanNats,
    db: PgPool,
    subscription_manager: Arc<SubscriptionManager>,
) {
    // Ensure the CATALOGUE stream exists
    if let Err(e) = nats.ensure_catalogue_stream().await {
        tracing::error!("failed to create CATALOGUE stream: {e}");
        return;
    }

    let consumer = match nats.catalogue_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create catalogue consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start catalogue message stream: {e}");
            return;
        }
    };

    tracing::info!("catalogue ingest started on catalogue.commands.register");

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("catalogue ingest message error: {e}");
                continue;
            }
        };

        // Extract NATS msg ID for dedup
        let nats_msg_id = msg
            .headers
            .as_ref()
            .and_then(|h| h.get("Nats-Msg-Id"))
            .map(|v| v.to_string());

        let cmd: CatalogueRegisterCommand = match serde_json::from_slice(&msg.payload) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("catalogue ingest: failed to deserialize command: {e}");
                let _ = msg.ack().await;
                continue;
            }
        };

        let user_metadata = serde_json::to_value(&cmd.user_metadata).unwrap_or_default();
        let file_metadata = cmd.file_metadata.clone().unwrap_or_default();
        let size_bytes = cmd.size_bytes.map(|s| s as i64);

        let result = sqlx::query(
            r#"
            INSERT INTO catalogue_entries (
                id, execution_id, job_id, name, category, filename,
                mime_type, size_bytes, storage_path,
                source_net, source_place, correlation_id, process_id, process_step,
                trace_id, file_metadata, user_metadata, created_at, nats_msg_id
            ) VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9,
                $10, $11, $12, $13, $14,
                $15, $16, $17, $18, $19
            )
            ON CONFLICT (nats_msg_id) DO NOTHING
            "#,
        )
        .bind(&cmd.artifact_id)
        .bind(&cmd.execution_id)
        .bind(&cmd.job_id)
        .bind(&cmd.name)
        .bind(&cmd.category)
        .bind(&cmd.filename)
        .bind(&cmd.mime_type)
        .bind(size_bytes)
        .bind(&cmd.storage_path)
        .bind(&cmd.source_net)
        .bind(&cmd.source_place)
        .bind(&cmd.correlation_id)
        .bind(&cmd.process_id)
        .bind(&cmd.process_step)
        .bind(&cmd.trace_id)
        .bind(&file_metadata)
        .bind(&user_metadata)
        .bind(cmd.created_at)
        .bind(&nats_msg_id)
        .execute(&db)
        .await;

        match result {
            Ok(r) => {
                if r.rows_affected() > 0 {
                    tracing::debug!(
                        artifact_id = %cmd.artifact_id,
                        execution_id = %cmd.execution_id,
                        "catalogued artifact",
                    );
                }
            }
            Err(e) => {
                tracing::error!(
                    artifact_id = %cmd.artifact_id,
                    execution_id = %cmd.execution_id,
                    "catalogue insert failed: {e}",
                );
                // NAK to retry later
                let _ = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        std::time::Duration::from_secs(2),
                    )))
                    .await;
                continue;
            }
        }

        let _ = msg.ack().await;

        // Evaluate the new artifact against active subscriptions
        let entry = command_to_entry(&cmd);
        subscription_manager.evaluate_new_artifact(&entry).await;
    }

    tracing::warn!("catalogue ingest stream ended");
}
