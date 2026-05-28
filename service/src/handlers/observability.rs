//! Observability endpoints.
//!
//! Currently just the silent-drop DLQ inspector
//! (`GET /api/v1/observability/silent-drops`). Reads the `MEKHAN_SILENT_DROPS`
//! JetStream stream via an ephemeral pull consumer and returns the
//! captured [`SilentDropRecord`]s — what each silent-drop call site
//! ACKed and dropped, with the raw payload + per-site context that
//! make the failure replayable.

use async_nats::jetstream::{self, consumer::DeliverPolicy};
use axum::{extract::Query, extract::State, Json};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use utoipa::ToSchema;

use crate::models::error::ApiError;
use crate::observability::{silent_drops, SilentDropRecord};
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct SilentDropsQuery {
    /// Maximum number of records to return. Hard-capped at 1000 so a
    /// single request can't drain the entire 10k-msg stream into a
    /// response body. Default: 100.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Filter by `kind` prefix. Empty / absent returns all kinds. The
    /// stream has a per-kind subject (`mekhan.silent_drops.{kind}`), so
    /// we apply the filter at the broker.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SilentDropsResponse {
    /// Process-wide counter — total drops since boot. Independent of
    /// the stream contents (which may have rolled off via retention).
    pub total_since_boot: u64,
    /// Captured records, newest first. May be empty if the stream is
    /// empty or the filter excludes everything.
    pub records: Vec<SilentDropRecord>,
}

/// `GET /api/v1/observability/silent-drops`
///
/// Reads the dead-letter queue: every record any consumer ACKed and
/// dropped because it couldn't parse the input. Drains up to `limit`
/// recent records from `MEKHAN_SILENT_DROPS` and returns them along
/// with the process-wide counter.
///
/// The stream itself enforces retention (currently 7d / 10k msgs); this
/// endpoint just exposes whatever is currently retained.
#[utoipa::path(
    get,
    path = "/api/v1/observability/silent-drops",
    params(
        ("limit" = Option<usize>, Query, description = "Max records to return (default 100, hard cap 1000)"),
        ("kind" = Option<String>, Query, description = "Filter by kind (broker-side subject filter)")
    ),
    responses(
        (status = 200, description = "Counter + recent dead-letter records", body = SilentDropsResponse),
    ),
    tag = "observability",
)]
pub async fn list_silent_drops(
    State(state): State<AppState>,
    Query(q): Query<SilentDropsQuery>,
) -> Result<Json<SilentDropsResponse>, ApiError> {
    let limit = q.limit.unwrap_or(100).min(1000);
    let filter_subject = match q.kind.as_deref() {
        None | Some("") => "mekhan.silent_drops.>".to_string(),
        Some(k) => format!("mekhan.silent_drops.{k}"),
    };

    let js = state.nats.jetstream().clone();
    let stream = match js.get_stream("MEKHAN_SILENT_DROPS").await {
        Ok(s) => s,
        Err(e) => {
            // Stream not yet created (early boot / fresh deployment) →
            // return the counter with an empty list rather than 500.
            let err_str = e.to_string();
            if err_str.contains("stream not found") || err_str.contains("10059") {
                return Ok(Json(SilentDropsResponse {
                    total_since_boot: silent_drops(),
                    records: vec![],
                }));
            }
            return Err(ApiError::internal(format!(
                "MEKHAN_SILENT_DROPS get_stream: {e}"
            )));
        }
    };

    // Ephemeral pull consumer scoped to this request. New each call so
    // we always replay the live retained set; no shared state to drift.
    // DeliverPolicy::All replays from the oldest retained record; we'll
    // sort newest-first below.
    let consumer = stream
        .create_consumer(jetstream::consumer::pull::Config {
            filter_subject,
            deliver_policy: DeliverPolicy::All,
            ack_policy: jetstream::consumer::AckPolicy::None,
            ..Default::default()
        })
        .await
        .map_err(|e| ApiError::internal(format!("create silent-drops consumer: {e}")))?;

    let mut messages = consumer
        .messages()
        .await
        .map_err(|e| ApiError::internal(format!("silent-drops messages stream: {e}")))?;

    let mut records: Vec<SilentDropRecord> = Vec::new();
    let read_timeout = Duration::from_millis(500);
    while records.len() < limit {
        match tokio::time::timeout(read_timeout, messages.next()).await {
            Ok(Some(Ok(msg))) => match serde_json::from_slice::<SilentDropRecord>(&msg.payload) {
                Ok(rec) => records.push(rec),
                Err(e) => {
                    // The stream itself carries a malformed record (shouldn't
                    // happen — we control the producer). Log and continue
                    // rather than fail the whole query.
                    tracing::warn!(
                        "silent-drops stream carried a record we couldn't parse: {e}"
                    );
                }
            },
            Ok(Some(Err(e))) => {
                tracing::warn!("silent-drops message read error: {e}");
                break;
            }
            Ok(None) => break, // stream ended
            Err(_) => break,   // 500ms idle → caught up
        }
    }

    // Newest first — the stream replays oldest-first.
    records.reverse();

    Ok(Json(SilentDropsResponse {
        total_since_boot: silent_drops(),
        records,
    }))
}
