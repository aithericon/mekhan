use std::sync::Arc;

use chrono::Utc;
use futures::StreamExt;
use serde::Deserialize;
use sqlx::PgPool;

use petri_domain::{DomainEvent, PersistedEvent};

use crate::catalogue::model::CatalogueRegisterCommand;
use crate::catalogue::subscriptions::SubscriptionManager;
use crate::nats::MekhanNats;

/// Slim serde type for CrossNetTokenTransfer messages on `petri.bridge.>`.
/// Avoids depending on `petri-nats` crate.
#[derive(Debug, Deserialize)]
struct CrossNetTokenTransfer {
    source_net_id: String,
    signal_key: String,
}

/// Start the causality event ingest consumer.
///
/// Subscribes to `petri.events.>` and `petri.bridge.>` on the `PETRI_GLOBAL`
/// JetStream stream and projects each domain event into the causality tables.
pub async fn start_causality_ingest(
    nats: MekhanNats,
    db: PgPool,
    subscription_manager: Arc<SubscriptionManager>,
) {
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
            process_domain_event(&db, subject, &msg.payload, &subscription_manager).await
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
        "INSERT INTO causality_cross_links (signal_key, ingress_net, link_type) \
         VALUES ($1, $2, 'bridge') \
         ON CONFLICT (signal_key) DO UPDATE SET ingress_net = $2",
    )
    .bind(&transfer.signal_key)
    .bind(target_net)
    .execute(db)
    .await?;

    tracing::debug!(
        signal_key = %transfer.signal_key,
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
    subscription_manager: &SubscriptionManager,
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
            process_step_started,
            process_step_completed,
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
            let read_ids: Vec<String> = read_tokens.iter().map(|(_, t)| t.id.0.to_string()).collect();
            let produced_ids: Vec<String> = produced_tokens.iter().map(|(_, t)| t.id.0.to_string()).collect();
            propagate_process_tags(db, &consumed_ids, &read_ids, &produced_ids).await?;

            // Project step breadcrumbs: if this transition is annotated with
            // process_step_started/process_step_completed, record the step event
            // against each process that the consumed/read tokens belong to.
            if process_step_started.is_some() || process_step_completed.is_some() {
                record_step_event(
                    db,
                    &consumed_ids,
                    &read_ids,
                    process_step_started.as_deref(),
                    process_step_completed.as_deref(),
                    ts,
                )
                .await?;
            }
        }

        DomainEvent::EffectCompleted {
            transition_id,
            transition_name,
            consumed_tokens,
            produced_tokens,
            effect_handler_id,
            effect_result,
            read_tokens,
            process_step_started,
            process_step_completed,
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
            let read_ids: Vec<String> = read_tokens.iter().map(|(_, t)| t.id.0.to_string()).collect();
            let produced_ids: Vec<String> = produced_tokens.iter().map(|(_, t)| t.id.0.to_string()).collect();
            propagate_process_tags(db, &consumed_ids, &read_ids, &produced_ids).await?;

            // Check effect_result for signal_key → egress cross-link
            if let Some(signal_key) = effect_result.get("signal_key").and_then(|v| v.as_str()) {
                sqlx::query(
                    "INSERT INTO causality_cross_links (signal_key, egress_net, egress_seq, link_type) \
                     VALUES ($1, $2, $3, 'effect') \
                     ON CONFLICT (signal_key) DO UPDATE SET egress_net = $2, egress_seq = $3",
                )
                .bind(signal_key)
                .bind(net_id)
                .bind(seq)
                .execute(db)
                .await?;
            }

            // Enrich auto-discovered processes with process_start metadata.
            // The process_start effect can fire at any point in the process lifecycle,
            // not just at the start. Its consumed/read tokens carry process tags
            // from seed tokens. We update those process rows with the name,
            // description, and steps from the effect result.
            if effect_handler_id == "process_start" {
                enrich_processes_from_start_event(db, &consumed_ids, &read_ids, effect_result, ts).await?;
            }

            // Mark process as completed when process_complete effect fires.
            if effect_handler_id == "process_complete" {
                complete_processes(db, &consumed_ids, &read_ids, ts).await?;
            }

            // Breadcrumb: log metric effect → write to hpi_metrics
            if effect_handler_id == "process_log_metric" {
                record_metric_event(db, &consumed_ids, &read_ids, effect_result, ts).await?;
            }

            // Breadcrumb: log message effect → write to hpi_logs
            if effect_handler_id == "process_log_message" {
                record_log_event(db, &consumed_ids, &read_ids, effect_result, ts).await?;
            }

            // Breadcrumb: human task effect → write to hpi_tasks
            if effect_handler_id == "human_task" {
                record_task_event(db, &consumed_ids, &read_ids, effect_result, ts).await?;
            }

            // Catalogue registration: the effect_result IS the full
            // CatalogueRegisterCommand. Resolve provenance from our
            // causality context and insert directly.
            if effect_handler_id == "catalogue_register" {
                register_catalogue_entry(
                    db,
                    net_id,
                    seq,
                    &consumed_ids,
                    &read_ids,
                    effect_result,
                    process_step_completed.as_deref()
                        .or(process_step_started.as_deref()),
                    ts,
                    subscription_manager,
                ).await?;
            }

            // Step breadcrumb (same as TransitionFired)
            if process_step_started.is_some() || process_step_completed.is_some() {
                record_step_event(
                    db,
                    &consumed_ids,
                    &read_ids,
                    process_step_started.as_deref(),
                    process_step_completed.as_deref(),
                    ts,
                )
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
                propagate_process_tags(db, &consumed_ids, &[], &produced_ids).await?;
            }
        }

        DomainEvent::TokenCreated {
            token,
            place_id,
            place_name,
            signal_key,
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

            if let Some(ref sk) = signal_key {
                // Token arrived via signal injection or bridge transfer.
                // The signal_key links back to the originating process via cross-links.
                sqlx::query(
                    "UPDATE causality_cross_links SET ingress_net = $2, ingress_seq = $3 \
                     WHERE signal_key = $1",
                )
                .bind(sk)
                .bind(net_id)
                .bind(seq)
                .execute(db)
                .await?;

                // Inherit process tags from the egress side.
                // For EffectCompleted (executor_submit): consumed tokens carry the process.
                // For TokenBridgedOut (bridge): the produced token carries the process.
                // We check both roles to handle both cases.
                let copied = sqlx::query(
                    "INSERT INTO causality_process_tags (token_id, process_id) \
                     SELECT $1, pt.process_id \
                     FROM causality_cross_links cl \
                     JOIN causality_event_tokens et \
                         ON et.net_id = cl.egress_net AND et.event_seq = cl.egress_seq \
                     JOIN causality_process_tags pt ON pt.token_id = et.token_id \
                     WHERE cl.signal_key = $2 \
                     ON CONFLICT DO NOTHING",
                )
                .bind(&token_id_str)
                .bind(sk)
                .execute(db)
                .await?;

                if copied.rows_affected() > 0 {
                    tracing::debug!(
                        token_id = %token_id_str,
                        signal_key = %sk,
                        "inherited process tags via signal_key",
                    );
                }

                // If this signal_key matches a pending task, mark it completed.
                // The signal_key for human task completion/cancellation is the
                // task_id, set by the global_human_result_listener when it
                // injects the result token.
                let status = extract_task_status_from_token(&token.color);
                sqlx::query(
                    "UPDATE hpi_tasks SET status = $2, completed_at = $3 \
                     WHERE id = $1 AND status = 'pending'",
                )
                .bind(sk)
                .bind(&status)
                .bind(ts)
                .execute(db)
                .await?;
            } else if token.created_by_event.is_none() {
                // True seed token (scenario initialization — no signal, no parent event).
                // Self-tag as process root and auto-create HPI process.
                sqlx::query(
                    "INSERT INTO causality_process_tags (token_id, process_id) \
                     VALUES ($1, $1) \
                     ON CONFLICT DO NOTHING",
                )
                .bind(&token_id_str)
                .execute(db)
                .await?;

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
            // else: token produced by a transition (created_by_event is set) —
            // inherits process tags via propagate_process_tags() when the
            // TransitionFired/EffectCompleted event is processed.
        }

        DomainEvent::TokenBridgedOut {
            token,
            signal_key,
            produced_by_event,
            ..
        } => {
            // Record egress side of cross-link.
            // Point egress_seq to the TransitionFired that produced this bridge-out
            // (via produced_by_event), NOT to this TokenBridgedOut event.
            // The transition's consumed tokens carry the process tags we need for inheritance.
            let egress_seq = produced_by_event
                .map(|e| e as i64)
                .unwrap_or(seq);

            sqlx::query(
                "INSERT INTO causality_cross_links (signal_key, egress_net, egress_seq, link_type) \
                 VALUES ($1, $2, $3, 'bridge') \
                 ON CONFLICT (signal_key) DO UPDATE SET egress_net = $2, egress_seq = $3",
            )
            .bind(signal_key)
            .bind(net_id)
            .bind(egress_seq)
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

/// Enrich auto-discovered processes with metadata from a process_start effect.
///
/// The process_start handler creates a named HPI process with steps. Instead of
/// maintaining a separate process event ingest path, we extract that metadata here
/// and update the causality-discovered process rows directly.
async fn enrich_processes_from_start_event(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    effect_result: &serde_json::Value,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    let name = effect_result.get("name").and_then(|v| v.as_str());
    let config = effect_result.get("config").cloned()
        .or_else(|| {
            // Build config from individual fields if not bundled
            let mut cfg = serde_json::Map::new();
            if let Some(desc) = effect_result.get("description") {
                cfg.insert("description".to_string(), desc.clone());
            }
            if let Some(ns) = effect_result.get("namespace") {
                cfg.insert("namespace".to_string(), ns.clone());
            }
            if let Some(steps) = effect_result.get("steps") {
                cfg.insert("steps".to_string(), steps.clone());
            }
            if cfg.is_empty() { None } else { Some(serde_json::Value::Object(cfg)) }
        });

    // Find process_ids from consumed + read tokens
    let mut source_ids = consumed_ids.to_vec();
    source_ids.extend_from_slice(read_ids);

    if source_ids.is_empty() {
        return Ok(());
    }

    // Get distinct process_ids from these tokens
    let process_ids: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT process_id FROM causality_process_tags WHERE token_id = ANY($1)",
    )
    .bind(&source_ids)
    .fetch_all(db)
    .await?;

    for pid in &process_ids {
        sqlx::query(
            "UPDATE hpi_processes SET \
               name = COALESCE($2, name), \
               config = CASE WHEN $3::jsonb IS NOT NULL THEN $3::jsonb ELSE config END, \
               updated_at = $4 \
             WHERE process_id = $1",
        )
        .bind(pid)
        .bind(name)
        .bind(&config)
        .bind(ts)
        .execute(db)
        .await?;

        tracing::debug!(
            process_id = %pid,
            name = ?name,
            "enriched auto-discovered process with process_start metadata",
        );
    }

    Ok(())
}

/// Mark processes as completed when the process_complete effect fires.
///
/// Resolves process IDs from the consumed/read tokens and sets their status
/// to "completed" with the completion timestamp.
async fn complete_processes(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    let process_ids = resolve_process_ids(db, consumed_ids, read_ids).await?;
    for pid in &process_ids {
        sqlx::query(
            "UPDATE hpi_processes SET status = 'completed', updated_at = $2 WHERE process_id = $1",
        )
        .bind(pid)
        .bind(ts)
        .execute(db)
        .await?;

        tracing::info!(
            process_id = %pid,
            "marked process as completed",
        );
    }
    Ok(())
}

/// Propagate process tags from source tokens (consumed + read) to produced tokens.
///
/// Both consumed and read-arc tokens contribute their process tags to produced tokens.
/// Read arcs carry process context (e.g., campaign state), so excluding them would
/// break tag propagation when a transition consumes an untagged trigger (e.g., a
/// subscription signal) alongside a tagged read-arc context token.
async fn propagate_process_tags(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    produced_ids: &[String],
) -> Result<(), sqlx::Error> {
    if produced_ids.is_empty() {
        return Ok(());
    }

    let mut source_ids: Vec<String> = consumed_ids.to_vec();
    source_ids.extend_from_slice(read_ids);

    if source_ids.is_empty() {
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
        .bind(&source_ids)
        .execute(db)
        .await?;
    }

    Ok(())
}

/// Resolve the set of process IDs that the given tokens belong to.
/// Used by the breadcrumb projectors (step/metric/log) to find which
/// auto-discovered process to write events against.
async fn resolve_process_ids(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
) -> Result<Vec<String>, sqlx::Error> {
    let mut source_ids = consumed_ids.to_vec();
    source_ids.extend_from_slice(read_ids);
    if source_ids.is_empty() {
        return Ok(vec![]);
    }
    sqlx::query_scalar(
        "SELECT DISTINCT process_id FROM causality_process_tags WHERE token_id = ANY($1)",
    )
    .bind(&source_ids)
    .fetch_all(db)
    .await
}

/// Project a step breadcrumb into hpi_processes.config['step_events'].
///
/// Records step transitions (started/completed) against each process the
/// firing transition's tokens belong to. Updates the process's updated_at.
async fn record_step_event(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    step_started: Option<&str>,
    step_completed: Option<&str>,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    let process_ids = resolve_process_ids(db, consumed_ids, read_ids).await?;
    if process_ids.is_empty() {
        return Ok(());
    }

    for pid in &process_ids {
        let event = serde_json::json!({
            "timestamp": ts.to_rfc3339(),
            "started": step_started,
            "completed": step_completed,
        });
        // Append to config.step_events array; create it if missing.
        sqlx::query(
            "UPDATE hpi_processes SET \
               config = jsonb_set(\
                 COALESCE(config, '{}'::jsonb), \
                 '{step_events}', \
                 COALESCE(config->'step_events', '[]'::jsonb) || $2::jsonb, \
                 true), \
               updated_at = $3 \
             WHERE process_id = $1",
        )
        .bind(pid)
        .bind(&event)
        .bind(ts)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// Project a metric breadcrumb into hpi_metrics.
///
/// Extracts key/value from the effect_result of a `process_log_metric`
/// EffectCompleted event and writes a row per matching process.
async fn record_metric_event(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    effect_result: &serde_json::Value,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    let key = effect_result.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let value = effect_result.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    if key.is_empty() {
        return Ok(());
    }

    let process_ids = resolve_process_ids(db, consumed_ids, read_ids).await?;
    for pid in &process_ids {
        sqlx::query(
            "INSERT INTO hpi_metrics (process_id, key, value, timestamp) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(pid)
        .bind(key)
        .bind(value)
        .bind(ts)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// Project a log breadcrumb into hpi_logs.
///
/// Extracts level/source/message/detail from the effect_result of a
/// `process_log_message` EffectCompleted event and writes a row per
/// matching process.
async fn record_log_event(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    effect_result: &serde_json::Value,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    let level = effect_result.get("level").and_then(|v| v.as_str()).unwrap_or("info");
    let source = effect_result.get("source").and_then(|v| v.as_str());
    let message = effect_result.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let detail = effect_result.get("detail").cloned().unwrap_or(serde_json::json!({}));

    let process_ids = resolve_process_ids(db, consumed_ids, read_ids).await?;
    for pid in &process_ids {
        sqlx::query(
            "INSERT INTO hpi_logs (process_id, level, source, message, detail, timestamp) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(pid)
        .bind(level)
        .bind(source)
        .bind(message)
        .bind(&detail)
        .bind(ts)
        .execute(db)
        .await?;
    }
    Ok(())
}

/// Project a human task breadcrumb into hpi_tasks.
///
/// Extracts task_id, title, and routing info from a `human_task`
/// EffectCompleted event's effect_result. The task_id is used as the
/// row PK (not the auto-generated UUID) so completion events can update
/// it by task_id = signal_key.
async fn record_task_event(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    effect_result: &serde_json::Value,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    let task_id = match effect_result.get("task_id").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return Ok(()),
    };
    let title = effect_result
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled Task")
        .to_string();

    let process_ids = resolve_process_ids(db, consumed_ids, read_ids).await?;
    // Attach to the first resolved process (tasks belong to exactly one process)
    let process_id = match process_ids.first() {
        Some(pid) => pid.clone(),
        None => {
            tracing::debug!(task_id = %task_id, "no process tag found for task; skipping");
            return Ok(());
        }
    };

    // Build detail from the whole effect_result (net_id, place, response_subject, etc.)
    let detail = effect_result.clone();

    sqlx::query(
        "INSERT INTO hpi_tasks (id, process_id, title, status, detail, created_at) \
         VALUES ($1, $2, $3, 'pending', $4, $5) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(task_id)
    .bind(&process_id)
    .bind(&title)
    .bind(&detail)
    .bind(ts)
    .execute(db)
    .await?;

    tracing::debug!(
        task_id = %task_id,
        process_id = %process_id,
        "projected task from human_task EffectCompleted",
    );
    Ok(())
}

/// Extract a completion status from a task result token.
/// The token's `status` field ("completed", "cancelled", "failed") is set
/// by the global_human_result_listener when injecting the result.
fn extract_task_status_from_token(color: &petri_domain::TokenColor) -> String {
    if let petri_domain::TokenColor::Data(v) = color {
        if let Some(s) = v.get("status").and_then(|x| x.as_str()) {
            return s.to_string();
        }
    }
    "completed".to_string()
}

/// Register a catalogue entry directly from the causality projector.
///
/// The `catalogue_register` effect handler returns the full
/// `CatalogueRegisterCommand` as its effect_result. We deserialize it,
/// resolve provenance fields from the causality context (source_net from
/// the event's net_id, source_place from consumed token place, process_id
/// from process tags), and insert into `catalogue_entries`.
async fn register_catalogue_entry(
    db: &PgPool,
    net_id: &str,
    event_seq: i64,
    consumed_ids: &[String],
    read_ids: &[String],
    effect_result: &serde_json::Value,
    process_step: Option<&str>,
    _ts: chrono::DateTime<chrono::Utc>,
    subscription_manager: &SubscriptionManager,
) -> Result<(), sqlx::Error> {
    let cmd: CatalogueRegisterCommand = match serde_json::from_value(effect_result.clone()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("catalogue_register: failed to deserialize effect_result: {e}");
            return Ok(());
        }
    };

    // Resolve provenance from causality context
    let process_ids = resolve_process_ids(db, consumed_ids, read_ids).await?;
    let process_id = process_ids.into_iter().next();

    // source_place: look up the consumed token's place from causality_event_tokens.
    // The consumed token's place in the EffectCompleted is the place feeding the
    // catalogue_register transition — which is the executor lifecycle's catalogue_pending place.
    let source_place: Option<String> = if !consumed_ids.is_empty() {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT place_name FROM causality_event_tokens \
             WHERE token_id = $1 AND role = 'produced' \
             ORDER BY event_seq DESC LIMIT 1",
        )
        .bind(&consumed_ids[0])
        .fetch_optional(db)
        .await?
        .flatten()
    } else {
        None
    };

    // Use process_step from EffectCompleted annotation, falling back to command
    let step = process_step
        .map(|s| s.to_string())
        .or(cmd.process_step.clone());

    let user_metadata = serde_json::to_value(&cmd.user_metadata).unwrap_or_default();
    let file_metadata = cmd.file_metadata.clone().unwrap_or_default();
    let size_bytes = cmd.size_bytes.map(|s| s as i64);

    // Deterministic nats_msg_id for dedup (matches the engine's msg ID pattern)
    let nats_msg_id = format!("cat-{}-{}", cmd.execution_id, cmd.artifact_id);

    let result = sqlx::query(
        r#"
        INSERT INTO catalogue_entries (
            id, execution_id, job_id, name, category, filename,
            mime_type, size_bytes, storage_path,
            source_net, source_place, signal_key, process_id, process_step,
            source_event_sequence,
            file_metadata, user_metadata, created_at, nats_msg_id
        ) VALUES (
            $1, $2, $3, $4, $5, $6,
            $7, $8, $9,
            $10, $11, $12, $13, $14,
            $15,
            $16, $17, $18, $19
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
    .bind(net_id)                   // source_net: from the event's net_id
    .bind(&source_place)            // source_place: from token provenance
    .bind(&cmd.signal_key)
    .bind(&process_id)              // process_id: from causality process tags
    .bind(&step)                    // process_step: from effect annotation or command
    .bind(event_seq)                // source_event_sequence: direct causality pointer
    .bind(&file_metadata)
    .bind(&user_metadata)
    .bind(cmd.created_at)
    .bind(&nats_msg_id)
    .execute(db)
    .await;

    match result {
        Ok(r) => {
            if r.rows_affected() > 0 {
                tracing::debug!(
                    artifact_id = %cmd.artifact_id,
                    source_net = %net_id,
                    process_id = ?process_id,
                    "catalogued artifact from causality projector",
                );

                // Evaluate subscriptions with full provenance
                let entry = crate::catalogue::model::CatalogueEntry {
                    id: cmd.artifact_id.clone(),
                    execution_id: cmd.execution_id.clone(),
                    job_id: Some(cmd.job_id.clone()),
                    name: cmd.name.clone(),
                    category: cmd.category.clone(),
                    filename: cmd.filename.clone(),
                    mime_type: cmd.mime_type.clone(),
                    size_bytes,
                    storage_path: cmd.storage_path.clone(),
                    source_net: Some(net_id.to_string()),
                    source_place: source_place.clone(),
                    signal_key: cmd.signal_key.clone(),
                    process_id: process_id.clone(),
                    process_step: step.clone(),
                    source_event_sequence: Some(event_seq),
                    file_metadata,
                    user_metadata,
                    created_at: cmd.created_at,
                    catalogued_at: Utc::now(),
                };
                subscription_manager.evaluate_new_artifact(&entry).await;
            }
        }
        Err(e) => {
            tracing::error!(
                artifact_id = %cmd.artifact_id,
                "catalogue insert from causality projector failed: {e}",
            );
            return Err(e);
        }
    }

    Ok(())
}
