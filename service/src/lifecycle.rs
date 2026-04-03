use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream::AckKind;
use futures::StreamExt;
use sqlx::PgPool;
use tracing;

use crate::catalogue::subscriptions::SubscriptionManager;
use crate::config::CleanupConfig;
use crate::nats::MekhanNats;
use crate::petri::client::PetriClient;

/// Start the NATS lifecycle event listener.
/// Subscribes to `petri.events.mekhan-*.net.>` and updates the DB
/// when NetCompleted or NetCancelled events arrive.
pub async fn start_lifecycle_listener(
    nats: MekhanNats,
    db: PgPool,
    subscription_manager: Arc<SubscriptionManager>,
) {
    let consumer = match nats.lifecycle_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create lifecycle consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start lifecycle message stream: {e}");
            return;
        }
    };

    tracing::info!("lifecycle listener started on petri.events.mekhan-*.net.>");

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("lifecycle listener message error: {e}");
                continue;
            }
        };

        // Parse subject: petri.events.{net_id}.net.{event_type}
        let subject = msg.subject.as_str();
        let parts: Vec<&str> = subject.split('.').collect();

        if parts.len() < 5 {
            tracing::warn!("unexpected lifecycle subject format: {subject}");
            let _ = msg.ack().await;
            continue;
        }

        let net_id = parts[2];
        let event_type = parts[parts.len() - 1];

        match event_type {
            "completed" => {
                tracing::info!("net {net_id} completed");
                let result = sqlx::query(
                    "UPDATE workflow_instances SET status = 'completed', completed_at = NOW() WHERE net_id = $1 AND status = 'running'"
                )
                .bind(net_id)
                .execute(&db)
                .await;

                match result {
                    Ok(r) if r.rows_affected() == 0 => {
                        tracing::warn!("no running instance found for {net_id}, will retry");
                        let _ = msg.ack_with(AckKind::Nak(Some(Duration::from_secs(1)))).await;
                        continue;
                    }
                    Err(e) => {
                        tracing::error!("failed to update instance status for {net_id}: {e}");
                        let _ = msg.ack_with(AckKind::Nak(Some(Duration::from_secs(1)))).await;
                        continue;
                    }
                    Ok(_) => {}
                }

                subscription_manager.cleanup_net_subscriptions(net_id).await;
            }
            "cancelled" => {
                tracing::info!("net {net_id} cancelled");
                let result = sqlx::query(
                    "UPDATE workflow_instances SET status = 'cancelled', completed_at = NOW() WHERE net_id = $1 AND status = 'running'"
                )
                .bind(net_id)
                .execute(&db)
                .await;

                match result {
                    Ok(r) if r.rows_affected() == 0 => {
                        tracing::warn!("no running instance found for {net_id}, will retry");
                        let _ = msg.ack_with(AckKind::Nak(Some(Duration::from_secs(1)))).await;
                        continue;
                    }
                    Err(e) => {
                        tracing::error!("failed to update instance status for {net_id}: {e}");
                        let _ = msg.ack_with(AckKind::Nak(Some(Duration::from_secs(1)))).await;
                        continue;
                    }
                    Ok(_) => {}
                }

                subscription_manager.cleanup_net_subscriptions(net_id).await;
            }
            _ => {
                // Ignore created, initialized, etc.
            }
        }

        let _ = msg.ack().await;
    }

    tracing::warn!("lifecycle listener stream ended");
}

/// Start the background cleanup sweep task.
/// Periodically scans for finished instances past the retention window and cleans them up.
pub async fn start_cleanup_sweep(
    config: CleanupConfig,
    db: PgPool,
    nats: MekhanNats,
    petri: PetriClient,
) {
    let interval_secs = config.sweep_interval_minutes * 60;
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));

    tracing::info!(
        "cleanup sweep started: retention={}h, interval={}m, purge_events={}",
        config.retention_hours,
        config.sweep_interval_minutes,
        config.purge_events
    );

    loop {
        interval.tick().await;
        cleanup_finished_instances(&config, &db, &nats, &petri).await;
    }
}

async fn cleanup_finished_instances(
    config: &CleanupConfig,
    db: &PgPool,
    nats: &MekhanNats,
    petri: &PetriClient,
) {
    let retention_interval = format!("{} hours", config.retention_hours);

    // Find instances that have been finished longer than the retention window
    let stale: Vec<(uuid::Uuid, String)> = match sqlx::query_as(
        r#"
        SELECT id, net_id FROM workflow_instances
        WHERE status IN ('completed', 'failed', 'cancelled')
        AND completed_at < NOW() - $1::interval
        "#,
    )
    .bind(&retention_interval)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("cleanup sweep query failed: {e}");
            return;
        }
    };

    if stale.is_empty() {
        return;
    }

    tracing::info!("cleanup sweep: {} instances to clean up", stale.len());

    for (instance_id, net_id) in &stale {
        cleanup_net(net_id, nats, petri, config.purge_events).await;

        // Update status to archived
        if let Err(e) = sqlx::query(
            "UPDATE workflow_instances SET status = 'archived' WHERE id = $1",
        )
        .bind(instance_id)
        .execute(db)
        .await
        {
            tracing::error!("failed to archive instance {instance_id}: {e}");
        }
    }

    tracing::info!("cleanup sweep complete: {} instances archived", stale.len());
}

/// Clean up a single net's resources. All operations are idempotent.
pub async fn cleanup_net(
    net_id: &str,
    nats: &MekhanNats,
    petri: &PetriClient,
    purge_events: bool,
) {
    // Step 1: Remove from petri-lab in-memory registry
    if let Err(e) = petri.delete_net(net_id).await {
        tracing::warn!("cleanup: failed to delete net {net_id} from engine: {e}");
    }

    // Step 2: Delete KV_NET_METADATA entry
    if let Err(e) = nats.delete_net_metadata(net_id).await {
        tracing::warn!("cleanup: failed to delete metadata for {net_id}: {e}");
    }

    // Step 3: Delete KV_NET_ACTIVITY entry
    if let Err(e) = nats.delete_net_activity(net_id).await {
        tracing::warn!("cleanup: failed to delete activity for {net_id}: {e}");
    }

    // Step 4: Purge NATS event stream data
    if purge_events {
        if let Err(e) = nats.purge_net_events(net_id).await {
            tracing::warn!("cleanup: failed to purge events for {net_id}: {e}");
        }

        // Step 5: Purge NATS signal data
        if let Err(e) = nats.purge_net_signals(net_id).await {
            tracing::warn!("cleanup: failed to purge signals for {net_id}: {e}");
        }
    }
}
