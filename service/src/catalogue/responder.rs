//! NATS request-reply responder for catalogue queries.
//!
//! Subscribes to `catalogue.query.>` using core NATS (not JetStream) and
//! dispatches each request to the appropriate repository method based on the
//! subject suffix.
//!
//! This is synchronous RPC over NATS: a caller publishes a request to e.g.
//! `catalogue.query.list` with a reply subject, and this responder sends
//! the result back on that reply subject.

use std::sync::Arc;

use futures::StreamExt;

use crate::nats::MekhanNats;
use super::protocol::*;
use super::repository::CatalogueRepository;
use super::subscriptions::SubscriptionManager;

/// Start the catalogue NATS request-reply responder.
///
/// Runs indefinitely, processing incoming requests on `catalogue.query.>`.
/// Each request is handled in a spawned tokio task so that slow queries
/// do not block the subscription loop.
pub async fn start_catalogue_responder(
    nats: MekhanNats,
    repo: Arc<dyn CatalogueRepository>,
    subscription_manager: Arc<SubscriptionManager>,
) {
    let client = nats.client().clone();

    // Subscribe to all catalogue.* subjects so we can route query, subscribe,
    // and unsubscribe requests from a single subscription.
    let mut sub = match client.subscribe("catalogue.>").await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to subscribe to catalogue.>: {e}");
            return;
        }
    };

    tracing::info!("catalogue responder started on catalogue.>");

    while let Some(msg) = sub.next().await {
        let subject = msg.subject.to_string();

        // Skip JetStream command subjects — they are handled by the ingest consumer
        if subject.starts_with("catalogue.commands.") {
            continue;
        }

        let repo = Arc::clone(&repo);
        let sm = Arc::clone(&subscription_manager);
        let client = client.clone();

        tokio::spawn(async move {
            let reply_payload = if subject.starts_with("catalogue.query.") {
                let operation = subject
                    .strip_prefix("catalogue.query.")
                    .unwrap_or(&subject);

                match operation {
                    "list" => handle_list(&repo, &msg.payload).await,
                    "get" => handle_get(&repo, &msg.payload).await,
                    "lineage" => handle_lineage(&repo, &msg.payload).await,
                    "stats" => handle_stats(&repo, &msg.payload).await,
                    "stats-by-net" => handle_stats_by_net(&repo).await,
                    "distinct" => handle_distinct(&repo, &msg.payload).await,
                    "distinct-jsonb" => handle_distinct_jsonb(&repo, &msg.payload).await,
                    unknown => {
                        tracing::warn!(operation = %unknown, "catalogue responder: unknown operation");
                        serde_json::to_vec(&CatalogueResponse::<()>::err(format!(
                            "unknown operation: {unknown}"
                        )))
                        .unwrap_or_default()
                    }
                }
            } else if subject == "catalogue.subscribe" {
                handle_subscribe(&sm, &repo, &msg.payload).await
            } else if subject == "catalogue.unsubscribe" {
                handle_unsubscribe(&sm, &msg.payload).await
            } else {
                // Unknown subject — ignore silently (could be internal NATS traffic)
                return;
            };

            if let Some(reply) = msg.reply {
                if let Err(e) = client
                    .publish(reply, reply_payload.into())
                    .await
                {
                    tracing::warn!(
                        subject = %subject,
                        "catalogue responder: failed to reply: {e}"
                    );
                }
            } else {
                tracing::debug!(
                    subject = %subject,
                    "catalogue responder: no reply subject, dropping response"
                );
            }
        });
    }

    tracing::warn!("catalogue responder subscription ended");
}

async fn handle_list(repo: &Arc<dyn CatalogueRepository>, payload: &[u8]) -> Vec<u8> {
    let req: CatalogueQueryRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => {
            return serialize_err(format!("invalid request: {e}"));
        }
    };

    let params = req.into();
    match repo.list_entries(&params).await {
        Ok(paginated) => serde_json::to_vec(&CatalogueResponse::ok(paginated)).unwrap_or_default(),
        Err(e) => serialize_err(format!("query error: {e}")),
    }
}

async fn handle_get(repo: &Arc<dyn CatalogueRepository>, payload: &[u8]) -> Vec<u8> {
    let req: CatalogueGetRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => {
            return serialize_err(format!("invalid request: {e}"));
        }
    };

    match repo.get_entry(&req.execution_id, &req.id).await {
        Ok(entry) => serde_json::to_vec(&CatalogueResponse::ok(entry)).unwrap_or_default(),
        Err(e) => serialize_err(format!("query error: {e}")),
    }
}

async fn handle_lineage(repo: &Arc<dyn CatalogueRepository>, payload: &[u8]) -> Vec<u8> {
    let req: CatalogueLineageRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => {
            return serialize_err(format!("invalid request: {e}"));
        }
    };

    match repo.lineage_grouped(&req.process_id).await {
        Ok(response) => serde_json::to_vec(&CatalogueResponse::ok(response)).unwrap_or_default(),
        Err(e) => serialize_err(format!("query error: {e}")),
    }
}

async fn handle_stats(repo: &Arc<dyn CatalogueRepository>, payload: &[u8]) -> Vec<u8> {
    let req: CatalogueQueryRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => {
            return serialize_err(format!("invalid request: {e}"));
        }
    };

    let params = req.into();
    match repo.stats(&params).await {
        Ok(stats) => serde_json::to_vec(&CatalogueResponse::ok(stats)).unwrap_or_default(),
        Err(e) => serialize_err(format!("query error: {e}")),
    }
}

async fn handle_stats_by_net(repo: &Arc<dyn CatalogueRepository>) -> Vec<u8> {
    match repo.stats_by_net().await {
        Ok(stats) => serde_json::to_vec(&CatalogueResponse::ok(stats)).unwrap_or_default(),
        Err(e) => serialize_err(format!("query error: {e}")),
    }
}

async fn handle_distinct(repo: &Arc<dyn CatalogueRepository>, payload: &[u8]) -> Vec<u8> {
    let req: CatalogueDistinctRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => {
            return serialize_err(format!("invalid request: {e}"));
        }
    };

    match repo.distinct_values(&req.column).await {
        Ok(values) => serde_json::to_vec(&CatalogueResponse::ok(values)).unwrap_or_default(),
        Err(e) => serialize_err(format!("query error: {e}")),
    }
}

async fn handle_distinct_jsonb(repo: &Arc<dyn CatalogueRepository>, payload: &[u8]) -> Vec<u8> {
    let req: CatalogueDistinctJsonbRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => {
            return serialize_err(format!("invalid request: {e}"));
        }
    };

    match repo.distinct_jsonb_values(&req.column, &req.key).await {
        Ok(values) => serde_json::to_vec(&CatalogueResponse::ok(values)).unwrap_or_default(),
        Err(e) => serialize_err(format!("query error: {e}")),
    }
}

async fn handle_subscribe(
    sm: &Arc<SubscriptionManager>,
    repo: &Arc<dyn CatalogueRepository>,
    payload: &[u8],
) -> Vec<u8> {
    let req: SubscribeRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return serialize_err(format!("invalid request: {e}")),
    };

    match sm.subscribe(req, repo.as_ref()).await {
        Ok(subscription_id) => {
            serde_json::to_vec(&CatalogueResponse::ok(SubscribeResponse { subscription_id }))
                .unwrap_or_default()
        }
        Err(e) => serialize_err(format!("subscribe failed: {e}")),
    }
}

async fn handle_unsubscribe(
    sm: &Arc<SubscriptionManager>,
    payload: &[u8],
) -> Vec<u8> {
    let req: UnsubscribeRequest = match serde_json::from_slice(payload) {
        Ok(r) => r,
        Err(e) => return serialize_err(format!("invalid request: {e}")),
    };

    match sm.unsubscribe(&req.subscription_id).await {
        Ok(unsubscribed) => {
            serde_json::to_vec(&CatalogueResponse::ok(UnsubscribeResponse { unsubscribed }))
                .unwrap_or_default()
        }
        Err(e) => serialize_err(format!("unsubscribe failed: {e}")),
    }
}

fn serialize_err(msg: String) -> Vec<u8> {
    serde_json::to_vec(&CatalogueResponse::<()>::err(msg)).unwrap_or_default()
}
