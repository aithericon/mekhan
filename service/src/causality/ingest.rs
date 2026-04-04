use futures::StreamExt;
use serde::Deserialize;
use sqlx::PgPool;

use petri_domain::{DomainEvent, PersistedEvent};

use crate::nats::MekhanNats;

/// Slim serde type for CrossNetTokenTransfer messages on `petri.bridge.>`.
/// Avoids depending on `petri-nats` crate.
#[derive(Debug, Deserialize)]
struct CrossNetTokenTransfer {
    source_net_id: String,
    correlation_id: String,
}

/// Start the causality event ingest consumer.
///
/// Subscribes to `petri.events.>` and `petri.bridge.>` on the `PETRI_GLOBAL`
/// JetStream stream and projects each domain event into the causality tables.
pub async fn start_causality_ingest(nats: MekhanNats, db: PgPool) {
    let consumer = match nats.causality_consumer().await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create causality consumer: {e}");
            return;
        }
    };

    let mut messages = match consumer.messages().await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start causality message stream: {e}");
            return;
        }
    };

    tracing::info!("causality ingest started on petri.events.> + petri.bridge.>");

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("causality ingest message error: {e}");
                continue;
            }
        };

        let subject = msg.subject.as_str();

        let result = if subject.starts_with("petri.bridge.") {
            // Bridge transfer message: petri.bridge.{target_net_id}.{place_name}
            process_bridge_transfer(&db, subject, &msg.payload).await
        } else if subject.starts_with("petri.events.") {
            // Domain event: petri.events.{net_id}.{event_type...}
            process_domain_event(&db, subject, &msg.payload).await
        } else {
            tracing::warn!("causality ingest: unexpected subject: {subject}");
            Ok(())
        };

        match result {
            Ok(()) => {
                let _ = msg.ack().await;
            }
            Err(e) => {
                tracing::error!(subject = %subject, "causality processing failed: {e}");
                let _ = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        std::time::Duration::from_secs(2),
                    )))
                    .await;
            }
        }
    }

    tracing::warn!("causality ingest stream ended");
}

/// Process a bridge transfer message to record the ingress side of a cross-net link
/// and propagate process tags to the arriving token.
async fn process_bridge_transfer(
    db: &PgPool,
    subject: &str,
    payload: &[u8],
) -> Result<(), sqlx::Error> {
    let transfer: CrossNetTokenTransfer = match serde_json::from_slice(payload) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("causality: failed to deserialize bridge transfer: {e}");
            return Ok(());
        }
    };

    // Extract target net from subject: petri.bridge.{target_net_id}.{place_name}
    let parts: Vec<&str> = subject.split('.').collect();
    let target_net = parts.get(2).unwrap_or(&"unknown");

    // Record ingress side of cross-link. The egress side was recorded when
    // TokenBridgedOut was processed.
    sqlx::query(
        "INSERT INTO causality_cross_links (correlation_id, ingress_net, link_type) \
         VALUES ($1, $2, 'bridge') \
         ON CONFLICT (correlation_id) DO UPDATE SET ingress_net = $2",
    )
    .bind(&transfer.correlation_id)
    .bind(target_net)
    .execute(db)
    .await?;

    tracing::debug!(
        correlation_id = %transfer.correlation_id,
        source = %transfer.source_net_id,
        target = %target_net,
        "recorded bridge ingress cross-link",
    );

    Ok(())
}

async fn process_domain_event(
    db: &PgPool,
    subject: &str,
    payload: &[u8],
) -> Result<(), sqlx::Error> {
    // Extract net_id from subject: petri.events.{net_id}.{event_type...}
    let net_id = match subject.split('.').nth(2) {
        Some(id) => id,
        None => {
            tracing::warn!("causality: cannot extract net_id from subject: {subject}");
            return Ok(());
        }
    };

    let persisted: PersistedEvent = match serde_json::from_slice(payload) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("causality: failed to deserialize event: {e}");
            return Ok(());
        }
    };

    let seq = persisted.sequence as i64;
    let ts = persisted.timestamp;

    match &persisted.event {
        DomainEvent::TransitionFired {
            transition_id,
            transition_name,
            consumed_tokens,
            produced_tokens,
            read_tokens,
        } => {
            sqlx::query(
                "INSERT INTO causality_events (net_id, event_seq, event_type, transition_name, timestamp) \
                 VALUES ($1, $2, 'TransitionFired', $3, $4) \
                 ON CONFLICT (net_id, event_seq) DO NOTHING",
            )
            .bind(net_id)
            .bind(seq)
            .bind(transition_name.as_deref().unwrap_or(&transition_id.0))
            .bind(ts)
            .execute(db)
            .await?;

            for (place_id, token_id) in consumed_tokens {
                insert_event_token(db, net_id, seq, &token_id.0.to_string(), "consumed", &place_id.0, None).await?;
            }
            for (place_id, token) in produced_tokens {
                insert_event_token(db, net_id, seq, &token.id.0.to_string(), "produced", &place_id.0, None).await?;
            }
            for (place_id, token) in read_tokens {
                insert_event_token(db, net_id, seq, &token.id.0.to_string(), "read", &place_id.0, None).await?;
            }

            let consumed_ids: Vec<String> = consumed_tokens.iter().map(|(_, tid)| tid.0.to_string()).collect();
            let produced_ids: Vec<String> = produced_tokens.iter().map(|(_, t)| t.id.0.to_string()).collect();
            propagate_process_tags(db, &consumed_ids, &produced_ids).await?;
        }

        DomainEvent::EffectCompleted {
            transition_id,
            transition_name,
            consumed_tokens,
            produced_tokens,
            effect_handler_id,
            effect_result,
            read_tokens,
        } => {
            sqlx::query(
                "INSERT INTO causality_events (net_id, event_seq, event_type, transition_name, effect_handler, timestamp) \
                 VALUES ($1, $2, 'EffectCompleted', $3, $4, $5) \
                 ON CONFLICT (net_id, event_seq) DO NOTHING",
            )
            .bind(net_id)
            .bind(seq)
            .bind(transition_name.as_deref().unwrap_or(&transition_id.0))
            .bind(effect_handler_id)
            .bind(ts)
            .execute(db)
            .await?;

            for (place_id, token_id) in consumed_tokens {
                insert_event_token(db, net_id, seq, &token_id.0.to_string(), "consumed", &place_id.0, None).await?;
            }
            for (place_id, token) in produced_tokens {
                insert_event_token(db, net_id, seq, &token.id.0.to_string(), "produced", &place_id.0, None).await?;
            }
            for (place_id, token) in read_tokens {
                insert_event_token(db, net_id, seq, &token.id.0.to_string(), "read", &place_id.0, None).await?;
            }

            let consumed_ids: Vec<String> = consumed_tokens.iter().map(|(_, tid)| tid.0.to_string()).collect();
            let produced_ids: Vec<String> = produced_tokens.iter().map(|(_, t)| t.id.0.to_string()).collect();
            propagate_process_tags(db, &consumed_ids, &produced_ids).await?;

            // Check effect_result for signal_key → egress cross-link
            if let Some(signal_key) = effect_result.get("signal_key").and_then(|v| v.as_str()) {
                sqlx::query(
                    "INSERT INTO causality_cross_links (correlation_id, egress_net, egress_seq, link_type) \
                     VALUES ($1, $2, $3, 'effect') \
                     ON CONFLICT (correlation_id) DO UPDATE SET egress_net = $2, egress_seq = $3",
                )
                .bind(signal_key)
                .bind(net_id)
                .bind(seq)
                .execute(db)
                .await?;
            }
        }

        DomainEvent::EffectFailed {
            transition_id,
            transition_name,
            consumed_tokens,
            produced_tokens,
            effect_handler_id,
            tokens_consumed,
            ..
        } => {
            sqlx::query(
                "INSERT INTO causality_events (net_id, event_seq, event_type, transition_name, effect_handler, timestamp) \
                 VALUES ($1, $2, 'EffectFailed', $3, $4, $5) \
                 ON CONFLICT (net_id, event_seq) DO NOTHING",
            )
            .bind(net_id)
            .bind(seq)
            .bind(transition_name.as_deref().unwrap_or(&transition_id.0))
            .bind(effect_handler_id)
            .bind(ts)
            .execute(db)
            .await?;

            if *tokens_consumed {
                for (place_id, token_id) in consumed_tokens {
                    insert_event_token(db, net_id, seq, &token_id.0.to_string(), "consumed", &place_id.0, None).await?;
                }
                for (place_id, token) in produced_tokens {
                    insert_event_token(db, net_id, seq, &token.id.0.to_string(), "produced", &place_id.0, None).await?;
                }

                let consumed_ids: Vec<String> = consumed_tokens.iter().map(|(_, tid)| tid.0.to_string()).collect();
                let produced_ids: Vec<String> = produced_tokens.iter().map(|(_, t)| t.id.0.to_string()).collect();
                propagate_process_tags(db, &consumed_ids, &produced_ids).await?;
            }
        }

        DomainEvent::TokenCreated {
            token,
            place_id,
            place_name,
            ..
        } => {
            sqlx::query(
                "INSERT INTO causality_events (net_id, event_seq, event_type, timestamp) \
                 VALUES ($1, $2, 'TokenCreated', $3) \
                 ON CONFLICT (net_id, event_seq) DO NOTHING",
            )
            .bind(net_id)
            .bind(seq)
            .bind(ts)
            .execute(db)
            .await?;

            let token_id_str = token.id.0.to_string();
            insert_event_token(db, net_id, seq, &token_id_str, "produced", &place_id.0, place_name.as_deref()).await?;

            // Check if this token arrived via cross-net bridge by looking for
            // a pending cross-link with this net as ingress. If found, copy
            // process tags from the egress side; otherwise, self-tag as new process.
            //
            // We try to match via the most recent unlinked cross-link for this net.
            // The bridge transfer message (petri.bridge.>) records the ingress_net
            // before the TokenCreated event, so the cross-link should exist.
            let linked = sqlx::query_scalar::<_, String>(
                "SELECT cl.correlation_id \
                 FROM causality_cross_links cl \
                 WHERE cl.ingress_net = $1 AND cl.ingress_seq IS NULL \
                 ORDER BY cl.correlation_id \
                 LIMIT 1",
            )
            .bind(net_id)
            .fetch_optional(db)
            .await?;

            if let Some(correlation_id) = linked {
                // Cross-net arrival: update the cross-link with the ingress event seq
                sqlx::query(
                    "UPDATE causality_cross_links SET ingress_seq = $2 \
                     WHERE correlation_id = $1",
                )
                .bind(&correlation_id)
                .bind(seq)
                .execute(db)
                .await?;

                // Copy process tags from egress tokens
                sqlx::query(
                    "INSERT INTO causality_process_tags (token_id, process_id) \
                     SELECT $1, pt.process_id \
                     FROM causality_cross_links cl \
                     JOIN causality_event_tokens et \
                         ON et.net_id = cl.egress_net AND et.event_seq = cl.egress_seq AND et.role = 'produced' \
                     JOIN causality_process_tags pt ON pt.token_id = et.token_id \
                     WHERE cl.correlation_id = $2 \
                     ON CONFLICT DO NOTHING",
                )
                .bind(&token_id_str)
                .bind(&correlation_id)
                .execute(db)
                .await?;

                tracing::debug!(
                    token_id = %token_id_str,
                    correlation_id = %correlation_id,
                    "linked bridged token to process via cross-link",
                );
            } else {
                // Seed token: self-tag as new process
                sqlx::query(
                    "INSERT INTO causality_process_tags (token_id, process_id) \
                     VALUES ($1, $1) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(&token_id_str)
                .execute(db)
                .await?;

                // Auto-create HPI process
                sqlx::query(
                    "INSERT INTO hpi_processes (process_id, status, created_at, updated_at) \
                     VALUES ($1, 'active', $2, $2) \
                     ON CONFLICT (process_id) DO NOTHING",
                )
                .bind(&token_id_str)
                .bind(ts)
                .execute(db)
                .await?;
            }
        }

        DomainEvent::TokenBridgedOut {
            token,
            signal_key,
            ..
        } => {
            // Record egress side of cross-link
            sqlx::query(
                "INSERT INTO causality_cross_links (correlation_id, egress_net, egress_seq, link_type) \
                 VALUES ($1, $2, $3, 'bridge') \
                 ON CONFLICT (correlation_id) DO UPDATE SET egress_net = $2, egress_seq = $3",
            )
            .bind(signal_key)
            .bind(net_id)
            .bind(seq)
            .execute(db)
            .await?;

            // Record as causality event
            sqlx::query(
                "INSERT INTO causality_events (net_id, event_seq, event_type, timestamp) \
                 VALUES ($1, $2, 'TokenBridgedOut', $3) \
                 ON CONFLICT (net_id, event_seq) DO NOTHING",
            )
            .bind(net_id)
            .bind(seq)
            .bind(ts)
            .execute(db)
            .await?;

            insert_event_token(db, net_id, seq, &token.id.0.to_string(), "produced", "", None).await?;
        }

        // Other event types are not relevant for token causality
        _ => {}
    }

    Ok(())
}

async fn insert_event_token(
    db: &PgPool,
    net_id: &str,
    event_seq: i64,
    token_id: &str,
    role: &str,
    place_id: &str,
    place_name: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO causality_event_tokens (net_id, event_seq, token_id, role, place_id, place_name) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT DO NOTHING",
    )
    .bind(net_id)
    .bind(event_seq)
    .bind(token_id)
    .bind(role)
    .bind(place_id)
    .bind(place_name)
    .execute(db)
    .await?;
    Ok(())
}

/// Propagate process tags from consumed tokens to produced tokens.
async fn propagate_process_tags(
    db: &PgPool,
    consumed_ids: &[String],
    produced_ids: &[String],
) -> Result<(), sqlx::Error> {
    if consumed_ids.is_empty() || produced_ids.is_empty() {
        return Ok(());
    }

    for produced_id in produced_ids {
        sqlx::query(
            "INSERT INTO causality_process_tags (token_id, process_id) \
             SELECT $1, pt.process_id \
             FROM causality_process_tags pt \
             WHERE pt.token_id = ANY($2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(produced_id)
        .bind(consumed_ids)
        .execute(db)
        .await?;
    }

    Ok(())
}
