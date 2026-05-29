//! Cross-cutting observability primitives.
//!
//! ## Silent drops
//!
//! NATS consumers (causality ingest, lifecycle, petri history scan, KV
//! hydration paths…) all share the same dilemma: a deserialization
//! failure can't be NAK'd back into the queue because retry is
//! deterministic — it would loop forever. Historically each consumer
//! responded with `tracing::warn!` + ACK, which is correct semantics but
//! invisible: an operator with no log alerting in place never notices,
//! and tests can't write a regression guard without grepping logs.
//!
//! This module centralises the response. Every silent-drop site goes
//! through [`record_silent_drop`] or [`record_silent_drop_with`], which:
//!
//!   1. Atomically increment the process-wide [`SILENT_DROPS`] counter
//!      (fast in-memory aggregate, used by tests + at-a-glance health).
//!   2. Emit a structured `tracing::error!` at target
//!      `mekhan_service::observability::silent_drop` (greppable /
//!      alertable from a single rule).
//!   3. Push a [`SilentDropRecord`] onto an in-process channel for the
//!      background drainer task ([`drain_silent_drops`]) to publish to
//!      the `MEKHAN_SILENT_DROPS` JetStream stream — the actual
//!      dead-letter queue, queryable via
//!      `GET /api/v1/observability/silent-drops`.
//!
//! The channel is wired at service boot ([`install_drainer`] →
//! [`drain_silent_drops`]). Tests don't install the drainer; the records
//! pile up briefly in the channel and the test assertions read the
//! counter directly. Production installs the drainer at startup and
//! every record reaches the stream.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use aithericon_executor_domain::sanitize_subject_token;

use crate::nats::MekhanNats;

/// Bounded retention but not bounded sends — the drainer reads at NATS
/// publish rate and we don't want call sites to block on a slow broker.
/// If NATS is down the channel grows; the cap is the process memory.
/// Acceptable because silent drops are (in healthy operation) rare.
static DRAIN_TX: OnceLock<UnboundedSender<SilentDropRecord>> = OnceLock::new();

static SILENT_DROPS: AtomicU64 = AtomicU64::new(0);

/// One dead-letter record. Published as a JSON message on
/// `mekhan.silent_drops.{kind}`.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SilentDropRecord {
    /// Stable identifier of the consumer + reason. Doubles as the NATS
    /// subject suffix (`mekhan.silent_drops.{kind}`) so consumers can
    /// filter at the broker. Examples: `catalogue_register`,
    /// `event_envelope`, `lifecycle_subject`,
    /// `catalogue_subscription_hydrate`.
    pub kind: String,
    /// Human-readable failure description — the underlying error
    /// (deser error, missing field, subject pattern mismatch, …).
    pub error: String,
    /// UTC timestamp the drop was recorded at.
    pub recorded_at: DateTime<Utc>,
    /// Per-site structured context. Free-form JSON object — typically
    /// `subject`, `net_id`, `key`, `event_seq`, whatever the call site
    /// knows that would help forensic inspection.
    #[serde(default, skip_serializing_if = "is_null_or_empty_object")]
    pub context: serde_json::Value,
    /// Raw payload the consumer couldn't parse, captured as a UTF-8
    /// string (lossy if the bytes weren't valid UTF-8). All current
    /// call sites carry JSON, so lossy is fine in practice; the loss
    /// only shows up if a malformed message contained binary noise.
    /// `None` for sites that drop on subject-only checks (no payload
    /// to capture).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
}

fn is_null_or_empty_object(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Null => true,
        serde_json::Value::Object(m) => m.is_empty(),
        _ => false,
    }
}

/// Total silent drops since process start (or last reset).
pub fn silent_drops() -> u64 {
    SILENT_DROPS.load(Ordering::Relaxed)
}

/// Reset the counter — exclusively for tests that want a clean baseline.
/// Production code should never call this.
pub fn reset_silent_drops() {
    SILENT_DROPS.store(0, Ordering::Relaxed);
}

/// Lean record: just `kind` + `error`. Use [`record_silent_drop_with`]
/// to attach the per-site context + raw payload that make a stream
/// record actionable in forensics.
pub fn record_silent_drop(kind: &str, error: &dyn std::fmt::Display) {
    record_silent_drop_with(kind, error, serde_json::Value::Null, None);
}

/// Rich record. `context` is a free-form JSON object (subject, net_id,
/// key, …); `payload` is the raw bytes the consumer couldn't parse.
pub fn record_silent_drop_with(
    kind: &str,
    error: &dyn std::fmt::Display,
    context: serde_json::Value,
    payload: Option<&[u8]>,
) {
    SILENT_DROPS.fetch_add(1, Ordering::Relaxed);
    let error_string = error.to_string();
    tracing::error!(
        target: "mekhan_service::observability::silent_drop",
        kind = kind,
        error = %error_string,
        "silent drop — malformed input ACKed and dropped"
    );

    // Best-effort enqueue for the drainer. If the drainer hasn't been
    // installed yet (boot order, tests) or the receiver was dropped,
    // the record is lost — the counter + log have already fired so the
    // signal isn't lost.
    if let Some(tx) = DRAIN_TX.get() {
        let record = SilentDropRecord {
            kind: kind.to_string(),
            error: error_string,
            recorded_at: Utc::now(),
            context,
            payload: payload.map(|b| String::from_utf8_lossy(b).into_owned()),
        };
        let _ = tx.send(record);
    }
}

/// Install the drainer's sender into the process-wide slot. Safe to call
/// at most once at service boot; subsequent calls are no-ops and the
/// returned [`UnboundedReceiver`] of the *first* call is the only live
/// drain stream.
pub fn install_drainer() -> Option<UnboundedReceiver<SilentDropRecord>> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    if DRAIN_TX.set(tx).is_err() {
        // Already installed — caller is double-initialising. The earlier
        // receiver is the one in use; this one would receive nothing.
        return None;
    }
    Some(rx)
}

/// Drain `rx` into the `MEKHAN_SILENT_DROPS` JetStream stream forever.
/// Each record is JSON-serialised onto `mekhan.silent_drops.{kind}` so a
/// consumer can filter by kind at the broker. NATS publish failures are
/// logged but never block the channel — a stream outage shouldn't break
/// the call sites that are themselves reporting a different failure.
pub async fn drain_silent_drops(
    nats: MekhanNats,
    mut rx: UnboundedReceiver<SilentDropRecord>,
) {
    let js = nats.jetstream().clone();
    while let Some(record) = rx.recv().await {
        let subject = format!("mekhan.silent_drops.{}", sanitize_subject_token(&record.kind));
        let bytes = match serde_json::to_vec(&record) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(
                    kind = %record.kind,
                    "silent drop record failed to serialise: {e}"
                );
                continue;
            }
        };
        if let Err(e) = js.publish(subject, bytes.into()).await {
            tracing::error!(
                kind = %record.kind,
                "silent drop record failed to publish: {e}"
            );
        }
    }
    tracing::warn!("silent drop drainer exited (channel closed)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_records_and_resets() {
        reset_silent_drops();
        assert_eq!(silent_drops(), 0);

        record_silent_drop("test_kind_a", &"first failure");
        assert_eq!(silent_drops(), 1);

        record_silent_drop_with(
            "test_kind_b",
            &"second failure",
            serde_json::json!({ "subject": "petri.events.foo" }),
            Some(b"raw payload bytes"),
        );
        assert_eq!(silent_drops(), 2);

        reset_silent_drops();
        assert_eq!(silent_drops(), 0);
    }

    #[test]
    fn sanitize_subject_token_keeps_safe_chars() {
        assert_eq!(sanitize_subject_token("catalogue_register"), "catalogue_register");
        assert_eq!(sanitize_subject_token("foo-bar_baz123"), "foo-bar_baz123");
    }

    #[test]
    fn sanitize_subject_token_replaces_unsafe_chars() {
        assert_eq!(sanitize_subject_token("foo.bar"), "foo_bar");
        assert_eq!(sanitize_subject_token("a>b"), "a_b");
        assert_eq!(sanitize_subject_token("space y"), "space_y");
    }

    #[test]
    fn record_serializes_with_lossy_payload_string() {
        // Non-UTF-8 bytes mid-payload survive via `from_utf8_lossy` (the
        // 0xFF gets replaced with U+FFFD). Real call sites always carry
        // JSON, but the test guards the worst case.
        record_silent_drop_with(
            "test_lossy",
            &"oops",
            serde_json::json!({}),
            Some(&[0x7B, 0xFF, 0x7D]), // `{\xFF}`
        );
        // We can't easily inspect the queued record without the drainer
        // installed; this just proves the call doesn't panic.
        assert!(silent_drops() >= 1);
    }
}
