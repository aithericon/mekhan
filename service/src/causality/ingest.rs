use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use serde::Deserialize;
use sqlx::PgPool;

use aithericon_executor_domain::{Phase, PhaseStatus, Progress, StatusDetail};
use petri_domain::{DomainEvent, PersistedEvent};

use crate::catalogue::model::CatalogueRegisterCommand;
use crate::catalogue::subscriptions::SubscriptionManager;
use crate::causality::live::LiveBroadcasts;
use crate::nats::MekhanNats;
use crate::triggers::TriggerDispatcher;

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
    live: Arc<LiveBroadcasts>,
    triggers: Option<Arc<TriggerDispatcher>>,
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
            process_domain_event(
                &db,
                subject,
                &msg.payload,
                &subscription_manager,
                &live,
                triggers.as_deref(),
            )
            .await
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
    live: &LiveBroadcasts,
    triggers: Option<&TriggerDispatcher>,
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
            process_step_started: _,
            process_step_completed: _,
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
                insert_event_token(db, net_id, seq, &token_id.0.to_string(), "consumed", &place_id.0, None, None).await?;
            }
            for (place_id, token) in produced_tokens {
                let data = token_color_to_json(&token.color);
                insert_event_token(db, net_id, seq, &token.id.0.to_string(), "produced", &place_id.0, None, Some(&data)).await?;
            }
            for (place_id, token) in read_tokens {
                let data = token_color_to_json(&token.color);
                insert_event_token(db, net_id, seq, &token.id.0.to_string(), "read", &place_id.0, None, Some(&data)).await?;
            }

            let consumed_ids: Vec<String> = consumed_tokens.iter().map(|(_, tid)| tid.0.to_string()).collect();
            let read_ids: Vec<String> = read_tokens.iter().map(|(_, t)| t.id.0.to_string()).collect();
            let produced_ids: Vec<String> = produced_tokens.iter().map(|(_, t)| t.id.0.to_string()).collect();
            propagate_process_tags(db, &consumed_ids, &read_ids, &produced_ids).await?;
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
                "INSERT INTO causality_events (net_id, event_seq, event_type, transition_name, effect_handler, effect_result, timestamp) \
                 VALUES ($1, $2, 'EffectCompleted', $3, $4, $5, $6) \
                 ON CONFLICT (net_id, event_seq) DO NOTHING",
            )
            .bind(net_id)
            .bind(seq)
            .bind(transition_name.as_deref().unwrap_or(&transition_id.0))
            .bind(effect_handler_id)
            .bind(effect_result)
            .bind(ts)
            .execute(db)
            .await?;

            for (place_id, token_id) in consumed_tokens {
                insert_event_token(db, net_id, seq, &token_id.0.to_string(), "consumed", &place_id.0, None, None).await?;
            }
            for (place_id, token) in produced_tokens {
                let data = token_color_to_json(&token.color);
                insert_event_token(db, net_id, seq, &token.id.0.to_string(), "produced", &place_id.0, None, Some(&data)).await?;
            }
            for (place_id, token) in read_tokens {
                let data = token_color_to_json(&token.color);
                insert_event_token(db, net_id, seq, &token.id.0.to_string(), "read", &place_id.0, None, Some(&data)).await?;
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

                // Record dispatch lineage: the FIRST EffectCompleted that emitted this
                // signal_key wins. cross_links above gets overwritten by later effects
                // that reuse the same key (e.g. executor_submit emits key K, then
                // catalogue_register at the end of the lifecycle reuses K). The
                // dispatches table preserves the original dispatcher so signal-injected
                // TokenCreated events can trace back to where the work began.
                sqlx::query(
                    "INSERT INTO causality_signal_dispatches \
                         (signal_key, dispatch_net, dispatch_seq) \
                     VALUES ($1, $2, $3) \
                     ON CONFLICT (signal_key) DO NOTHING",
                )
                .bind(signal_key)
                .bind(net_id)
                .bind(seq)
                .execute(db)
                .await?;
            }

            // Dispatch the effect_handler_id-keyed causality side-effects.
            // The registry owns the id→projector mapping; an unknown id is a
            // visible no-op (`None`), not a silently-missing ladder arm.
            if let Some(projector) = projector_for(effect_handler_id) {
                let ctx = ProjectorCtx {
                    db,
                    net_id,
                    seq,
                    consumed_ids: &consumed_ids,
                    read_ids: &read_ids,
                    effect_result,
                    process_step: process_step_completed
                        .as_deref()
                        .or(process_step_started.as_deref()),
                    ts,
                    subscription_manager,
                    live,
                    triggers,
                };
                projector.project(&ctx).await?;
            }
        }

        DomainEvent::EffectFailed {
            transition_id,
            transition_name,
            consumed_tokens,
            produced_tokens,
            effect_handler_id,
            error_message,
            tokens_consumed,
            input_data,
            retryable,
            ..
        } => {
            // Capture the failure as effect_result so the UI can show why it failed.
            let failure_result = serde_json::json!({
                "error_message": error_message,
                "retryable": retryable,
                "input_data": input_data,
                "tokens_consumed": tokens_consumed,
            });
            sqlx::query(
                "INSERT INTO causality_events (net_id, event_seq, event_type, transition_name, effect_handler, effect_result, timestamp) \
                 VALUES ($1, $2, 'EffectFailed', $3, $4, $5, $6) \
                 ON CONFLICT (net_id, event_seq) DO NOTHING",
            )
            .bind(net_id)
            .bind(seq)
            .bind(transition_name.as_deref().unwrap_or(&transition_id.0))
            .bind(effect_handler_id)
            .bind(&failure_result)
            .bind(ts)
            .execute(db)
            .await?;

            if *tokens_consumed {
                for (place_id, token_id) in consumed_tokens {
                    insert_event_token(db, net_id, seq, &token_id.0.to_string(), "consumed", &place_id.0, None, None).await?;
                }
                for (place_id, token) in produced_tokens {
                    let data = token_color_to_json(&token.color);
                    insert_event_token(db, net_id, seq, &token.id.0.to_string(), "produced", &place_id.0, None, Some(&data)).await?;
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
            let token_data = token_color_to_json(&token.color);
            insert_event_token(db, net_id, seq, &token_id_str, "produced", &place_id.0, place_name.as_deref(), Some(&token_data)).await?;

            if let Some(ref sk) = signal_key {
                // Token arrived via signal injection or bridge transfer.
                // The signal_key links back to the originating process via cross-links.
                let updated = sqlx::query(
                    "UPDATE causality_cross_links SET ingress_net = $2, ingress_seq = $3 \
                     WHERE signal_key = $1",
                )
                .bind(sk)
                .bind(net_id)
                .bind(seq)
                .execute(db)
                .await?;

                // Record signal lineage: every signal-injected TokenCreated whose
                // signal_key matches a known dispatch (recorded in
                // causality_signal_dispatches) gets a row in causality_signal_lineage
                // pointing back to the dispatch event. This handles the N:1 case
                // where one executor_submit produces many status/event signals.
                // PK is (ingress_net, ingress_seq) so each signal arrival is unique.
                sqlx::query(
                    "INSERT INTO causality_signal_lineage \
                         (ingress_net, ingress_seq, dispatch_net, dispatch_seq, signal_key) \
                     SELECT $1, $2, sd.dispatch_net, sd.dispatch_seq, sd.signal_key \
                     FROM causality_signal_dispatches sd \
                     WHERE sd.signal_key = $3 \
                       AND NOT (sd.dispatch_net = $1 AND sd.dispatch_seq = $2) \
                     ON CONFLICT (ingress_net, ingress_seq) DO NOTHING",
                )
                .bind(net_id)
                .bind(seq)
                .bind(sk)
                .execute(db)
                .await?;

                // Backfill fallback: catalogue subscription signals published by
                // `run_backfill` (at subscription-creation time) run outside the
                // ingest consumer, so no egress-side row exists. Detect this by
                // checking if the UPDATE affected 0 rows and the signal_key
                // matches the `cat-sub:` convention, then resolve the source
                // artifact from `catalogue_entries` and INSERT the full row.
                if updated.rows_affected() == 0 && sk.starts_with("cat-sub:") {
                    let parts: Vec<&str> = sk.splitn(4, ':').collect();
                    if parts.len() == 4 {
                        let exec_id = parts[2];
                        let art_id = parts[3];
                        sqlx::query(
                            "INSERT INTO causality_cross_links \
                                 (signal_key, egress_net, egress_seq, \
                                  ingress_net, ingress_seq, link_type) \
                             SELECT $1, ce.source_net, ce.source_event_sequence, \
                                    $2, $3, 'catalogue_subscription' \
                             FROM catalogue_entries ce \
                             WHERE ce.execution_id = $4 AND ce.id = $5 \
                               AND ce.source_net IS NOT NULL \
                               AND ce.source_event_sequence IS NOT NULL \
                             ON CONFLICT (signal_key) DO UPDATE \
                             SET ingress_net = $2, ingress_seq = $3",
                        )
                        .bind(sk)
                        .bind(net_id)
                        .bind(seq)
                        .bind(exec_id)
                        .bind(art_id)
                        .execute(db)
                        .await?;
                    }
                }

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

                // The seed token carries the instance it was parameterised
                // for (injected in petri::instance). Record it so a process
                // links back to its instance/net; backfill if the row already
                // exists from an earlier re-ingest with no instance.
                let instance_id = token_data
                    .get("_instance_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let proc_net_id = instance_id.as_ref().map(|i| format!("mekhan-{i}"));

                sqlx::query(
                    "INSERT INTO hpi_processes \
                         (process_id, status, instance_id, net_id, created_at, updated_at) \
                     VALUES ($1, 'active', $3::uuid, $4, $2, $2) \
                     ON CONFLICT (process_id) DO UPDATE \
                     SET instance_id = COALESCE(hpi_processes.instance_id, EXCLUDED.instance_id), \
                         net_id = COALESCE(hpi_processes.net_id, EXCLUDED.net_id)",
                )
                .bind(&token_id_str)
                .bind(ts)
                .bind(&instance_id)
                .bind(&proc_net_id)
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
            target_net_id,
            target_place_name,
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

            // Record as causality event, including bridge target so the UI
            // can render "Bridge → {target_net} / {target_place}" in the
            // event detail sheet without re-parsing the domain event.
            sqlx::query(
                "INSERT INTO causality_events (net_id, event_seq, event_type, bridge_target_net, bridge_target_place, timestamp) \
                 VALUES ($1, $2, 'TokenBridgedOut', $3, $4, $5) \
                 ON CONFLICT (net_id, event_seq) DO NOTHING",
            )
            .bind(net_id)
            .bind(seq)
            .bind(target_net_id)
            .bind(target_place_name)
            .bind(ts)
            .execute(db)
            .await?;

            let bridge_data = token_color_to_json(&token.color);
            insert_event_token(db, net_id, seq, &token.id.0.to_string(), "produced", "", None, Some(&bridge_data)).await?;
        }

        // Other event types are not relevant for token causality
        _ => {}
    }

    Ok(())
}

/// Decoded `EffectCompleted` envelope handed to a [`Projector`].
///
/// `process_domain_event` does the generic envelope work (insert the
/// `causality_events` row, the event tokens, propagate process tags, record
/// signal cross-links) and then asks the registry for the projector matching
/// `effect_handler_id`. Everything an `effect_handler_id`-keyed projector
/// could need is bundled here so dispatch is a single call. Lifetimes are all
/// borrowed from the `process_domain_event` frame — `ProjectorCtx` is built,
/// used, and dropped within one event.
struct ProjectorCtx<'a> {
    db: &'a PgPool,
    net_id: &'a str,
    seq: i64,
    consumed_ids: &'a [String],
    read_ids: &'a [String],
    effect_result: &'a serde_json::Value,
    /// `process_step_completed` falling back to `process_step_started`, the
    /// exact precedence the old inline `catalogue_register` arm used.
    process_step: Option<&'a str>,
    ts: chrono::DateTime<chrono::Utc>,
    subscription_manager: &'a SubscriptionManager,
    live: &'a LiveBroadcasts,
    triggers: Option<&'a TriggerDispatcher>,
}

/// One `effect_handler_id` → causality side-effect mapping.
///
/// Each implementation owns exactly the block that used to live behind an
/// `if effect_handler_id == "..."` guard in `process_domain_event`. The
/// typed `process_phase` / `process_progress` ids each map to their own
/// projector that deserializes the whole `StatusDetail` — no magic-string
/// folding onto the log/metric projectors. The registry ([`projector_for`])
/// makes the set of handled ids explicit: an unknown id resolves to `None`
/// and is a no-op by construction, not silently lost in a ladder.
#[async_trait]
trait Projector: Sync {
    async fn project(&self, ctx: &ProjectorCtx<'_>) -> Result<(), sqlx::Error>;
}

struct ProcessStart;
#[async_trait]
impl Projector for ProcessStart {
    async fn project(&self, ctx: &ProjectorCtx<'_>) -> Result<(), sqlx::Error> {
        // The process_start effect can fire at any point in the process
        // lifecycle, not just at the start. Its consumed/read tokens carry
        // process tags from seed tokens. We update those process rows with
        // the name, description, and steps from the effect result.
        enrich_processes_from_start_event(
            ctx.db,
            ctx.consumed_ids,
            ctx.read_ids,
            ctx.effect_result,
            ctx.ts,
        )
        .await
    }
}

struct ProcessComplete;
#[async_trait]
impl Projector for ProcessComplete {
    async fn project(&self, ctx: &ProjectorCtx<'_>) -> Result<(), sqlx::Error> {
        complete_processes(ctx.db, ctx.consumed_ids, ctx.read_ids, ctx.ts).await
    }
}

struct ProcessFail;
#[async_trait]
impl Projector for ProcessFail {
    async fn project(&self, ctx: &ProjectorCtx<'_>) -> Result<(), sqlx::Error> {
        // The net keeps running to its normal End — this is a process-level
        // marker, not a net kill-switch — so workflow_instances.status is
        // intentionally left untouched.
        fail_processes(
            ctx.db,
            ctx.consumed_ids,
            ctx.read_ids,
            ctx.effect_result,
            ctx.ts,
        )
        .await
    }
}

struct ProcessLogMetric;
#[async_trait]
impl Projector for ProcessLogMetric {
    async fn project(&self, ctx: &ProjectorCtx<'_>) -> Result<(), sqlx::Error> {
        record_metric_event(
            ctx.db,
            ctx.consumed_ids,
            ctx.read_ids,
            ctx.effect_result,
            ctx.ts,
            ctx.live,
        )
        .await
    }
}

struct ProcessPhase;
#[async_trait]
impl Projector for ProcessPhase {
    async fn project(&self, ctx: &ProjectorCtx<'_>) -> Result<(), sqlx::Error> {
        // effect_result IS the verbatim serialized StatusDetail. Deserialize
        // the whole typed value and project the PhaseChanged variant.
        project_phase_status_detail(
            ctx.db,
            ctx.consumed_ids,
            ctx.read_ids,
            ctx.effect_result,
            ctx.ts,
        )
        .await
    }
}

struct ProcessProgress;
#[async_trait]
impl Projector for ProcessProgress {
    async fn project(&self, ctx: &ProjectorCtx<'_>) -> Result<(), sqlx::Error> {
        // effect_result IS the verbatim serialized StatusDetail. Deserialize
        // the whole typed value and project the ProgressUpdated variant.
        project_progress_status_detail(
            ctx.db,
            ctx.consumed_ids,
            ctx.read_ids,
            ctx.effect_result,
            ctx.ts,
        )
        .await
    }
}

struct ProcessLogMessage;
#[async_trait]
impl Projector for ProcessLogMessage {
    async fn project(&self, ctx: &ProjectorCtx<'_>) -> Result<(), sqlx::Error> {
        record_log_event(
            ctx.db,
            ctx.consumed_ids,
            ctx.read_ids,
            ctx.effect_result,
            ctx.ts,
            ctx.live,
        )
        .await
    }
}

struct HumanTask;
#[async_trait]
impl Projector for HumanTask {
    async fn project(&self, ctx: &ProjectorCtx<'_>) -> Result<(), sqlx::Error> {
        record_task_event(
            ctx.db,
            ctx.consumed_ids,
            ctx.read_ids,
            ctx.effect_result,
            ctx.ts,
        )
        .await
    }
}

struct CatalogueRegister;
#[async_trait]
impl Projector for CatalogueRegister {
    async fn project(&self, ctx: &ProjectorCtx<'_>) -> Result<(), sqlx::Error> {
        // The effect_result IS the full CatalogueRegisterCommand. Resolve
        // provenance from our causality context and insert directly.
        register_catalogue_entry(
            ctx.db,
            ctx.net_id,
            ctx.seq,
            ctx.consumed_ids,
            ctx.read_ids,
            ctx.effect_result,
            ctx.process_step,
            ctx.ts,
            ctx.subscription_manager,
            ctx.live,
            ctx.triggers,
        )
        .await
    }
}

/// Registry: map an `effect_handler_id` to the projector that owns its
/// causality side-effects, or `None` when no projector handles it.
///
/// This is the single place the handled-id set is declared. Adding a
/// projector means adding one arm here; a missing projector is a visible
/// `None` (a structural no-op) rather than an arm silently absent from a
/// long `if` ladder.
fn projector_for(effect_handler_id: &str) -> Option<&'static dyn Projector> {
    match effect_handler_id {
        "process_start" => Some(&ProcessStart),
        "process_complete" => Some(&ProcessComplete),
        "process_fail" => Some(&ProcessFail),
        "process_phase" => Some(&ProcessPhase),
        "process_progress" => Some(&ProcessProgress),
        "process_log_metric" => Some(&ProcessLogMetric),
        "process_log_message" => Some(&ProcessLogMessage),
        "human_task" => Some(&HumanTask),
        "catalogue_register" => Some(&CatalogueRegister),
        _ => None,
    }
}

/// Convert a TokenColor into the most useful JSON representation for UI
/// display: `Data(v)` → `v`, `Integer(n)` → number, `Unit` → null.
/// The type tag is elided — consumers of the provenance detail endpoint
/// care about the payload, not whether it was a Unit/Integer/Data marker.
fn token_color_to_json(color: &petri_domain::TokenColor) -> serde_json::Value {
    match color {
        petri_domain::TokenColor::Unit => serde_json::Value::Null,
        petri_domain::TokenColor::Integer(n) => serde_json::Value::from(*n),
        petri_domain::TokenColor::Data(v) => v.clone(),
    }
}

async fn insert_event_token(
    db: &PgPool,
    net_id: &str,
    event_seq: i64,
    token_id: &str,
    role: &str,
    place_id: &str,
    place_name: Option<&str>,
    token_data: Option<&serde_json::Value>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO causality_event_tokens (net_id, event_seq, token_id, role, place_id, place_name, token_data) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT DO NOTHING",
    )
    .bind(net_id)
    .bind(event_seq)
    .bind(token_id)
    .bind(role)
    .bind(place_id)
    .bind(place_name)
    .bind(token_data)
    .execute(db)
    .await?;
    Ok(())
}

/// Enrich auto-discovered processes with metadata from a process_start effect.
///
/// The process_start handler creates a named HPI process with optional
/// description/namespace metadata. Instead of maintaining a separate process
/// event ingest path, we extract that metadata here and update the
/// causality-discovered process rows directly.
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

/// Mark processes as failed when the process_fail effect fires.
///
/// Resolves process IDs from the consumed/read tokens (same tag-graph path as
/// `complete_processes`) and sets their status to "failed", storing the
/// interpolated failure message under `config.failure` (mirroring
/// `write_progress`'s jsonb_set idiom). Empty resolution ⇒ graceful no-op
/// (the Failure node was used outside a named process).
async fn fail_processes(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    effect_result: &serde_json::Value,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    let process_ids = resolve_process_ids(db, consumed_ids, read_ids).await?;
    if process_ids.is_empty() {
        return Ok(());
    }
    let reason = effect_result
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let failure = serde_json::json!({ "message": reason, "failed_at": ts });
    for pid in &process_ids {
        sqlx::query(
            "UPDATE hpi_processes SET status = 'failed', \
               config = jsonb_set(COALESCE(config, '{}'::jsonb), '{failure}', $2::jsonb, true), \
               updated_at = $3 \
             WHERE process_id = $1",
        )
        .bind(pid)
        .bind(&failure)
        .bind(ts)
        .execute(db)
        .await?;

        tracing::info!(
            process_id = %pid,
            reason = %reason,
            "marked process as failed",
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

/// Resolve the set of processes for `consumed`+`read` tokens, returning early
/// from the *caller* when none resolve.
///
/// Every breadcrumb projector opens with the same three lines: resolve the
/// process IDs, bail if empty, then loop per process. This macro collapses the
/// first two — `let pids = resolved_or_done!(db, consumed, read);` — leaving
/// each projector with only its parse + per-pid write specifics. It expands to
/// a `Vec` binding (not a closure-driven loop) so each projector's
/// `await`-in-loop borrows stay trivial and behavior is byte-identical to the
/// hand-written form it replaces.
macro_rules! resolved_or_done {
    ($db:expr, $consumed:expr, $read:expr) => {{
        let pids = resolve_process_ids($db, $consumed, $read).await?;
        if pids.is_empty() {
            return Ok(());
        }
        pids
    }};
}

/// Load the canonical `Progress` from a process's `config.progress`, or a
/// fresh empty one. `config.progress` is serialized exactly as
/// `aithericon_executor_domain::Progress` — the single reconciled model.
async fn load_progress(
    db: &PgPool,
    process_id: &str,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<Progress, sqlx::Error> {
    let raw: Option<Option<serde_json::Value>> = sqlx::query_scalar(
        "SELECT config -> 'progress' FROM hpi_processes WHERE process_id = $1",
    )
    .bind(process_id)
    .fetch_optional(db)
    .await?;
    Ok(raw
        .flatten()
        .and_then(|v| serde_json::from_value::<Progress>(v).ok())
        .unwrap_or(Progress {
            fraction: 0.0,
            message: None,
            current_step: 0,
            total_steps: 0,
            phases: Vec::new(),
            updated_at: ts,
        }))
}

/// Persist the canonical `Progress` back into `config.progress` (create the
/// key if missing), using the same jsonb_set idiom as the other config writers.
async fn write_progress(
    db: &PgPool,
    process_id: &str,
    progress: &Progress,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    let value = serde_json::to_value(progress).unwrap_or_else(|_| serde_json::json!({}));
    sqlx::query(
        "UPDATE hpi_processes SET \
           config = jsonb_set(COALESCE(config, '{}'::jsonb), '{progress}', $2::jsonb, true), \
           updated_at = $3 \
         WHERE process_id = $1",
    )
    .bind(process_id)
    .bind(&value)
    .bind(ts)
    .execute(db)
    .await?;
    Ok(())
}

/// Project a typed `StatusDetail::PhaseChanged` into
/// `config.progress.phases`. Upserts the phase by name, preserving
/// first-seen order; sets `started_at` on first transition to `running`
/// and `ended_at` on a terminal status.
///
/// The whole `StatusDetail` is deserialized once by the projector — there
/// is no field-by-field reconstruction or magic-string detection here.
async fn record_phase_event(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    phase_name: &str,
    status: PhaseStatus,
    message: Option<&str>,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    if phase_name.is_empty() {
        return Ok(());
    }
    let message = message.map(str::to_string);
    let terminal = matches!(
        status,
        PhaseStatus::Completed | PhaseStatus::Failed | PhaseStatus::Skipped
    );

    let process_ids = resolved_or_done!(db, consumed_ids, read_ids);

    for pid in &process_ids {
        let mut progress = load_progress(db, pid, ts).await?;
        match progress.phases.iter_mut().find(|p| p.name == phase_name) {
            Some(ph) => {
                ph.status = status;
                if message.is_some() {
                    ph.message = message.clone();
                }
                if status == PhaseStatus::Running && ph.started_at.is_none() {
                    ph.started_at = Some(ts);
                }
                if terminal {
                    ph.ended_at = Some(ts);
                }
            }
            None => progress.phases.push(Phase {
                name: phase_name.to_string(),
                status,
                message: message.clone(),
                started_at: if status == PhaseStatus::Running {
                    Some(ts)
                } else {
                    None
                },
                ended_at: if terminal { Some(ts) } else { None },
            }),
        }
        progress.updated_at = ts;
        write_progress(db, pid, &progress, ts).await?;
    }
    Ok(())
}

/// Project a typed `StatusDetail::ProgressUpdated` into
/// `config.progress`, leaving `phases` untouched.
///
/// The whole `StatusDetail` is deserialized once by the projector — there
/// is no `progress_fraction` magic-string key match or field-by-field
/// reconstruction here.
async fn record_progress_event(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    fraction: f64,
    message: Option<&str>,
    current_step: u64,
    total_steps: u64,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    let message = message.map(str::to_string);

    let process_ids = resolved_or_done!(db, consumed_ids, read_ids);

    for pid in &process_ids {
        let mut progress = load_progress(db, pid, ts).await?;
        progress.fraction = fraction;
        if message.is_some() {
            progress.message = message.clone();
        }
        progress.current_step = current_step;
        progress.total_steps = total_steps;
        progress.updated_at = ts;
        write_progress(db, pid, &progress, ts).await?;
    }
    Ok(())
}

/// Deserialize an `effect_result` into a typed `StatusDetail` and project
/// the `PhaseChanged` variant. Any other variant (or a malformed payload)
/// is a structural no-op — there is no stringly fallback.
async fn project_phase_status_detail(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    effect_result: &serde_json::Value,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    if let Ok(StatusDetail::PhaseChanged {
        phase_name,
        status,
        message,
    }) = serde_json::from_value::<StatusDetail>(effect_result.clone())
    {
        record_phase_event(
            db,
            consumed_ids,
            read_ids,
            &phase_name,
            status,
            message.as_deref(),
            ts,
        )
        .await?;
    }
    Ok(())
}

/// Deserialize an `effect_result` into a typed `StatusDetail` and project
/// the `ProgressUpdated` variant. Any other variant (or a malformed
/// payload) is a structural no-op — there is no stringly fallback.
async fn project_progress_status_detail(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    effect_result: &serde_json::Value,
    ts: chrono::DateTime<chrono::Utc>,
) -> Result<(), sqlx::Error> {
    if let Ok(StatusDetail::ProgressUpdated {
        fraction,
        message,
        current_step,
        total_steps,
    }) = serde_json::from_value::<StatusDetail>(effect_result.clone())
    {
        record_progress_event(
            db,
            consumed_ids,
            read_ids,
            fraction,
            message.as_deref(),
            current_step,
            total_steps,
            ts,
        )
        .await?;
    }
    Ok(())
}

/// Project a metric breadcrumb into hpi_metrics.
///
/// Extracts key/value from the effect_result of a `process_log_metric`
/// EffectCompleted event and writes a row per matching process.
/// Resolve the signal_key that caused a metric/log/task-style effect to fire.
///
/// A `process_log_metric` or `process_log_message` effect fires when a
/// `sig_metric` or `sig_log` token is consumed. Those tokens were created
/// by an `ExternalSignal` injection — we stored that link in
/// `causality_signal_lineage` (ingress_net+ingress_seq → dispatch info,
/// keyed by signal_key). Walk back: consumed token → producer event
/// (TokenCreated) → signal_lineage.signal_key.
///
/// Returns `None` for metrics/logs whose upstream chain has no signal (e.g.
/// internally-generated seeds, scenario bootstraps).
async fn resolve_signal_key_from_consumed(
    db: &PgPool,
    consumed_ids: &[String],
) -> Result<Option<String>, sqlx::Error> {
    if consumed_ids.is_empty() {
        return Ok(None);
    }
    sqlx::query_scalar::<_, String>(
        "SELECT sl.signal_key \
         FROM causality_event_tokens et \
         JOIN causality_signal_lineage sl \
             ON sl.ingress_net = et.net_id AND sl.ingress_seq = et.event_seq \
         WHERE et.token_id = ANY($1) AND et.role = 'produced' \
         LIMIT 1",
    )
    .bind(consumed_ids)
    .fetch_optional(db)
    .await
}

async fn record_metric_event(
    db: &PgPool,
    consumed_ids: &[String],
    read_ids: &[String],
    effect_result: &serde_json::Value,
    ts: chrono::DateTime<chrono::Utc>,
    live: &LiveBroadcasts,
) -> Result<(), sqlx::Error> {
    let key = effect_result.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let value = effect_result.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    if key.is_empty() {
        return Ok(());
    }

    let process_ids = resolved_or_done!(db, consumed_ids, read_ids);
    let signal_key = resolve_signal_key_from_consumed(db, consumed_ids).await?;
    for pid in &process_ids {
        sqlx::query(
            "INSERT INTO hpi_metrics (process_id, key, value, timestamp, signal_key) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(pid)
        .bind(key)
        .bind(value)
        .bind(ts)
        .bind(&signal_key)
        .execute(db)
        .await?;

        live.emit_metric(
            pid.clone(),
            signal_key.clone(),
            key.to_string(),
            value,
            ts,
        );
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
    live: &LiveBroadcasts,
) -> Result<(), sqlx::Error> {
    let level = effect_result.get("level").and_then(|v| v.as_str()).unwrap_or("info");
    let source = effect_result.get("source").and_then(|v| v.as_str());
    let message = effect_result.get("message").and_then(|v| v.as_str()).unwrap_or("");
    let detail = effect_result.get("detail").cloned().unwrap_or(serde_json::json!({}));

    let process_ids = resolved_or_done!(db, consumed_ids, read_ids);
    let signal_key = resolve_signal_key_from_consumed(db, consumed_ids).await?;
    for pid in &process_ids {
        sqlx::query(
            "INSERT INTO hpi_logs (process_id, level, source, message, detail, timestamp, signal_key) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(pid)
        .bind(level)
        .bind(source)
        .bind(message)
        .bind(&detail)
        .bind(ts)
        .bind(&signal_key)
        .execute(db)
        .await?;

        live.emit_log(
            pid.clone(),
            signal_key.clone(),
            level.to_string(),
            source.map(|s| s.to_string()),
            message.to_string(),
            detail.clone(),
            ts,
        );
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
    live: &LiveBroadcasts,
    triggers: Option<&TriggerDispatcher>,
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

                // Fan out to live SSE subscribers once the row is committed.
                if let Some(pid) = process_id.as_ref() {
                    live.emit_artifact(
                        pid.clone(),
                        cmd.artifact_id.clone(),
                        cmd.execution_id.clone(),
                        cmd.name.clone(),
                        cmd.category.clone(),
                        cmd.filename.clone(),
                        cmd.mime_type.clone(),
                        cmd.storage_path.clone(),
                        size_bytes,
                        step.clone(),
                        cmd.signal_key.clone(),
                        user_metadata.clone(),
                        cmd.created_at,
                    );
                }

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
                let matched = subscription_manager.evaluate_new_artifact(&entry).await;

                // Catalog triggers (Phase 5c): fire any static `Catalog`
                // triggers whose filters match this entry. Static triggers
                // coexist with the engine's runtime `catalogue_subscribe`
                // effect — they share the same `CatalogueEntry` source of
                // truth, just author it differently.
                if let Some(dispatcher) = triggers {
                    crate::triggers::sources::catalog::evaluate(dispatcher, &entry).await;
                }

                // Record egress-side cross-links for every matched subscription.
                // The ingress side is filled in later by the TokenCreated handler
                // when the signal arrives in the subscriber net. Because the
                // causality ingest consumer is single-threaded, the egress row
                // is guaranteed to exist before any TokenCreated from the same
                // signal is processed.
                for m in &matched {
                    if let Err(e) = sqlx::query(
                        "INSERT INTO causality_cross_links \
                             (signal_key, egress_net, egress_seq, link_type) \
                         VALUES ($1, $2, $3, 'catalogue_subscription') \
                         ON CONFLICT (signal_key) DO UPDATE \
                         SET egress_net = $2, egress_seq = $3",
                    )
                    .bind(&m.signal_key)
                    .bind(net_id)
                    .bind(event_seq)
                    .execute(db)
                    .await
                    {
                        tracing::warn!(
                            signal_key = %m.signal_key,
                            target_net = %m.target_net_id,
                            "failed to record catalogue subscription cross-link: {e}"
                        );
                    }
                }
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
