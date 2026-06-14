use std::convert::Infallible;
use std::time::Duration;

use axum::{
    extract::State,
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::Stream;
use futures::StreamExt;

use crate::AppState;

/// GET /api/v1/tasks/stream — SSE endpoint for real-time task events from NATS.
///
/// Emits `{event, data}` lines for `task_created`, `task_completed`,
/// `task_failed`, `task_cancelled`, `process_update`. `data` is the raw
/// NATS payload as a JSON string (clients re-parse it).
#[utoipa::path(
    get,
    path = "/api/v1/tasks/stream",
    responses(
        (status = 200, description = "SSE stream of task lifecycle events", content_type = "text/event-stream"),
    ),
    tag = "tasks",
)]
pub async fn task_stream(
    State(state): State<AppState>,
    user: crate::auth::model::AuthUser,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let client = state.nats.client().clone();

    // An open inbox connection is the human's session liveness source: we
    // core-publish `human.{member}.presence` once on connect and on every ping
    // tick so the human presence controller (core-subscribed to
    // `human.*.presence`) renews this member's availability. The controller
    // reads the member from the SUBJECT — the payload is intentionally empty.
    let member = user.subject_as_uuid();
    let presence_subject = format!("human.{member}.presence");

    // Workspace scope: human.* subjects carry the instance net_id
    // `mekhan-{ws}-{inst}`, which embeds the producing workspace. We only relay
    // events whose net workspace matches the caller's — otherwise this firehose
    // leaks every tenant's task lifecycle (payloads included) to any session.
    let caller_ws = user.workspace_id.unwrap_or_else(uuid::Uuid::nil);

    let stream = async_stream::stream! {
        yield Ok(Event::default().event("connected").data("ok"));

        if let Err(e) = client.publish(presence_subject.clone(), Vec::new().into()).await {
            tracing::warn!("Failed to publish human presence heartbeat: {e}");
        }

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
                        if workspace_from_net_id(net_id) == Some(caller_ws) {
                            let data = String::from_utf8_lossy(&msg.payload);
                            yield Ok(Event::default().event("task_created").data(&*data));
                        }
                    }
                }
                Some(msg) = completed_sub.next() => {
                    if let Some(net_id) = extract_net_id(&msg.subject, "human.completed.") {
                        if workspace_from_net_id(net_id) == Some(caller_ws) {
                            let data = String::from_utf8_lossy(&msg.payload);
                            yield Ok(Event::default().event("task_completed").data(&*data));
                        }
                    }
                }
                Some(msg) = failed_sub.next() => {
                    if let Some(net_id) = extract_net_id(&msg.subject, "human.failed.") {
                        if workspace_from_net_id(net_id) == Some(caller_ws) {
                            let data = String::from_utf8_lossy(&msg.payload);
                            yield Ok(Event::default().event("task_failed").data(&*data));
                        }
                    }
                }
                Some(msg) = cancelled_sub.next() => {
                    if let Some(net_id) = extract_net_id(&msg.subject, "human.cancelled.") {
                        if workspace_from_net_id(net_id) == Some(caller_ws) {
                            let data = String::from_utf8_lossy(&msg.payload);
                            yield Ok(Event::default().event("task_cancelled").data(&*data));
                        }
                    }
                }
                Some(msg) = process_sub.next() => {
                    if let Some(namespace) = extract_net_id(&msg.subject, "human.process.") {
                        if workspace_from_net_id(namespace) == Some(caller_ws) {
                            let data = String::from_utf8_lossy(&msg.payload);
                            yield Ok(Event::default().event("process_update").data(&*data));
                        }
                    }
                }
                _ = ping_interval.tick() => {
                    // Renew this member's presence: an open inbox tab keeps the
                    // human available to the presence controller.
                    if let Err(e) = client.publish(presence_subject.clone(), Vec::new().into()).await {
                        tracing::warn!("Failed to publish human presence heartbeat: {e}");
                    }
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

/// Workspace UUID embedded in an instance net_id `mekhan-{ws}-{inst}` (both
/// UUIDs, five `-`-groups each). Returns `None` for non-`mekhan` or malformed
/// nets, so the caller's workspace match also acts as the old `mekhan-` prefix
/// gate (a stray non-instance subject is simply not relayed).
fn workspace_from_net_id(net_id: &str) -> Option<uuid::Uuid> {
    let rest = net_id.strip_prefix("mekhan-")?;
    let segs: Vec<&str> = rest.split('-').collect();
    if segs.len() < 10 {
        return None;
    }
    segs[0..5].join("-").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_parses_from_namespaced_net_id() {
        let ws = "11111111-1111-1111-1111-111111111111";
        let inst = "22222222-2222-2222-2222-222222222222";
        assert_eq!(
            workspace_from_net_id(&format!("mekhan-{ws}-{inst}")),
            Some(ws.parse().unwrap())
        );
    }

    #[test]
    fn workspace_rejects_legacy_and_foreign_nets() {
        // Legacy single-UUID net (pre multi-tenancy) carries no workspace.
        assert_eq!(
            workspace_from_net_id("mekhan-22222222-2222-2222-2222-222222222222"),
            None
        );
        assert_eq!(workspace_from_net_id("pool-abc"), None);
        assert_eq!(workspace_from_net_id("mekhan-short"), None);
    }
}
