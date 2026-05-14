use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::Stream;
use futures::StreamExt;

use crate::AppState;

/// GET /api/tasks/stream — SSE endpoint for real-time task events from NATS.
///
/// Emits `{event, data}` lines for `task_created`, `task_completed`,
/// `task_failed`, `task_cancelled`, `process_update`. `data` is the raw
/// NATS payload as a JSON string (clients re-parse it).
#[utoipa::path(
    get,
    path = "/api/tasks/stream",
    responses(
        (status = 200, description = "SSE stream of task lifecycle events", content_type = "text/event-stream"),
    ),
    tag = "tasks",
)]
pub async fn task_stream(
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let client = state.nats.client().clone();

    let stream = async_stream::stream! {
        yield Ok(Event::default().event("connected").data("ok"));

        // Use core NATS subscriptions (not JetStream) — we only want live events.
        let mut request_sub = match client.subscribe("human.request.>").await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to subscribe to human.request.>: {e}");
                yield Ok(Event::default().event("error").data(format!("NATS subscribe failed: {e}")));
                return;
            }
        };
        let mut completed_sub = match client.subscribe("human.completed.>").await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to subscribe to human.completed.>: {e}");
                return;
            }
        };
        let mut failed_sub = match client.subscribe("human.failed.>").await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to subscribe to human.failed.>: {e}");
                return;
            }
        };
        let mut cancelled_sub = match client.subscribe("human.cancelled.>").await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to subscribe to human.cancelled.>: {e}");
                return;
            }
        };
        let mut process_sub = match client.subscribe("human.process.>").await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("Failed to subscribe to human.process.>: {e}");
                return;
            }
        };

        let mut ping_interval = tokio::time::interval(Duration::from_secs(10));

        loop {
            tokio::select! {
                Some(msg) = request_sub.next() => {
                    if let Some(net_id) = extract_net_id(&msg.subject, "human.request.") {
                        if net_id.starts_with("mekhan-") {
                            let data = String::from_utf8_lossy(&msg.payload);
                            yield Ok(Event::default().event("task_created").data(data.into_owned()));
                        }
                    }
                }
                Some(msg) = completed_sub.next() => {
                    if let Some(net_id) = extract_net_id(&msg.subject, "human.completed.") {
                        if net_id.starts_with("mekhan-") {
                            let data = String::from_utf8_lossy(&msg.payload);
                            yield Ok(Event::default().event("task_completed").data(data.into_owned()));
                        }
                    }
                }
                Some(msg) = failed_sub.next() => {
                    if let Some(net_id) = extract_net_id(&msg.subject, "human.failed.") {
                        if net_id.starts_with("mekhan-") {
                            let data = String::from_utf8_lossy(&msg.payload);
                            yield Ok(Event::default().event("task_failed").data(data.into_owned()));
                        }
                    }
                }
                Some(msg) = cancelled_sub.next() => {
                    if let Some(net_id) = extract_net_id(&msg.subject, "human.cancelled.") {
                        if net_id.starts_with("mekhan-") {
                            let data = String::from_utf8_lossy(&msg.payload);
                            yield Ok(Event::default().event("task_cancelled").data(data.into_owned()));
                        }
                    }
                }
                Some(msg) = process_sub.next() => {
                    if let Some(namespace) = extract_net_id(&msg.subject, "human.process.") {
                        if namespace.starts_with("mekhan-") {
                            let data = String::from_utf8_lossy(&msg.payload);
                            yield Ok(Event::default().event("process_update").data(data.into_owned()));
                        }
                    }
                }
                _ = ping_interval.tick() => {
                    yield Ok(Event::default().comment("ping"));
                }
            }
        }
    };

    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10)))
}

/// Extract net_id from a NATS subject like "human.request.{net_id}.{place}"
fn extract_net_id<'a>(subject: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = subject.strip_prefix(prefix)?;
    rest.split('.').next()
}
