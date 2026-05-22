//! Cross-cutting observability primitives.
//!
//! ## Silent-drop counter
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
//! through [`record_silent_drop`], which:
//!
//!   1. Atomically increments the process-wide [`SILENT_DROPS`] counter.
//!   2. Emits a structured `tracing::error!` at target
//!      `mekhan_service::observability::silent_drop` with `kind` /
//!      `error` fields — greppable / alertable from a single rule.
//!
//! Tests assert [`silent_drops`] is `0` at teardown as a regression
//! guard, calling [`reset_silent_drops`] for a clean baseline. The
//! `kind` tag identifies which consumer dropped (e.g.
//! `"catalogue_register"`, `"lifecycle_envelope"`,
//! `"petri_events_history"`) so an alert from production tells you
//! exactly where to look.
//!
//! Not a replacement for a proper dead-letter subject, which is the
//! right long-term home for unparseable events. This is the minimum
//! viable loudness — visible enough to catch in CI and on a dashboard,
//! cheap enough to instrument everywhere.

use std::sync::atomic::{AtomicU64, Ordering};

static SILENT_DROPS: AtomicU64 = AtomicU64::new(0);

/// Total silent drops since process start (or last reset).
pub fn silent_drops() -> u64 {
    SILENT_DROPS.load(Ordering::Relaxed)
}

/// Reset the counter — exclusively for tests that want a clean baseline.
/// Production code should never call this.
pub fn reset_silent_drops() {
    SILENT_DROPS.store(0, Ordering::Relaxed);
}

/// Record one silent drop. `kind` identifies the consumer + reason
/// (stable string for grep / alert rules); `error` is the underlying
/// failure (deser error, missing field, subject pattern mismatch, …).
pub fn record_silent_drop(kind: &str, error: &dyn std::fmt::Display) {
    SILENT_DROPS.fetch_add(1, Ordering::Relaxed);
    tracing::error!(
        target: "mekhan_service::observability::silent_drop",
        kind = kind,
        error = %error,
        "silent drop — malformed input ACKed and dropped"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity: the basic record/read/reset triple works in isolation.
    /// Counter is process-wide so the test sequences its asserts under
    /// `reset_silent_drops` rather than running in parallel — running this
    /// alongside an e2e test that publishes a malformed event would see
    /// each other's bumps. The harness handles serialisation via
    /// `--test-threads`; the test just verifies the local contract.
    #[test]
    fn counter_records_and_resets() {
        reset_silent_drops();
        assert_eq!(silent_drops(), 0);

        record_silent_drop("test_kind_a", &"first failure");
        assert_eq!(silent_drops(), 1);

        record_silent_drop("test_kind_b", &"second failure");
        assert_eq!(silent_drops(), 2);

        reset_silent_drops();
        assert_eq!(silent_drops(), 0);
    }
}
