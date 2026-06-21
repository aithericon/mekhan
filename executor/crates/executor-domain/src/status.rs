use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Lifecycle status of an execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Accepted,
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}

impl ExecutionStatus {
    /// Whether this status represents a terminal state (no further transitions).
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::TimedOut
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
        }
    }
}

impl std::fmt::Display for ExecutionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Published to NATS on every status transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct StatusUpdate {
    /// The execution this update belongs to.
    pub execution_id: String,

    /// The workspace (tenant) this update belongs to, threaded from
    /// `ExecutionJob.workspace_id`. Inserted as a subject segment after the
    /// `executor.status` category root so the back-channel is tenant-attributable
    /// (and a future per-tenant watcher can edge-filter `executor.status.{ws}.>`)
    /// while the single `executor.status.>` stream still captures everything.
    ///
    /// `#[serde(default)]` so a status message published by an older worker (no
    /// `workspace_id` key) still deserializes.
    #[serde(default)]
    pub workspace_id: String,

    /// Current status.
    pub status: ExecutionStatus,

    /// Structured detail about the status (e.g., pid for Running, exit_code for Completed).
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub detail: serde_json::Value,

    /// Echoed from ExecutionJob.metadata — callers use this for routing.
    pub metadata: HashMap<String, String>,

    /// Which executor instance produced this update.
    pub source: String,

    /// When this update was produced.
    pub timestamp: DateTime<Utc>,
}

impl StatusUpdate {
    /// Build the NATS subject for this update.
    /// Pattern: `executor.status.{ws}.{execution_id}.{status}`
    ///
    /// The `{ws}` segment sits AFTER the `executor.status` category root so the
    /// existing stream subject `executor.status.>` (a trailing tail wildcard
    /// matching one-or-more tokens) still captures the now-5-token subject, with
    /// no stream-config change. `{ws}` is sanitized like `execution_id` so a
    /// workspace UUID is byte-stable and a stray dot/wildcard cannot inject an
    /// extra subject token.
    pub fn subject(&self) -> String {
        format!(
            "executor.status.{}.{}.{}",
            sanitize_subject_token(&self.workspace_id),
            sanitize_subject_token(&self.execution_id),
            self.status.as_str()
        )
    }

    /// Deterministic message ID for JetStream dedup.
    /// Each execution transitions through each status at most once.
    pub fn msg_id(&self) -> String {
        format!("{}-{}", self.execution_id, self.status.as_str())
    }
}

/// NATS subject a cancel request is published to for a single execution.
/// Pattern: `executor.cancel.{execution_id}`, carried on the JetStream
/// [`CANCEL_STREAM`] (NOT core NATS).
///
/// Cancels ride JetStream because core pub/sub interest does not propagate from
/// the internal NATS connection (mekhan/engine) to a runner connected over the
/// Traefik WebSocket front door — so a core publish to `executor.cancel.*` was
/// silently dropped before reaching the runner, while jobs/status/events (all
/// JetStream) crossed the boundary fine. JetStream delivery is interest-free:
/// the message lands in the stream and the runner's consumer pulls it.
///
/// NOTE: intentionally NOT run through `sanitize_subject_token`, to preserve the
/// publish subject byte-for-byte (the engine `CancelClient` and the test harness
/// both build this bare format). Aligning cancel/status sanitization is tracked
/// separately as audit item A3.
pub fn cancel_subject(execution_id: &str) -> String {
    format!("executor.cancel.{execution_id}")
}

/// JetStream stream that carries cancellation requests (`executor.cancel.*`).
/// See [`cancel_subject`] for why cancels are on JetStream rather than core NATS.
pub const CANCEL_STREAM: &str = "EXECUTOR_CANCEL";

/// Max age of a cancel message in [`CANCEL_STREAM`]. A cancel is a transient
/// "stop now" signal, so a short retention window caps replay: a runner that
/// (re)connects creates a fresh `DeliverPolicy::New` consumer and never
/// re-applies a stale cancel to a reused execution id, while the window is still
/// wide enough to absorb brief publisher↔consumer skew. 5 minutes.
pub const CANCEL_STREAM_MAX_AGE_SECS: u64 = 300;

/// JetStream stream name for cancellation, honoring the worker's optional
/// isolation prefix (mirrors the status/events stream naming convention):
/// `None` → `EXECUTOR_CANCEL`; `Some("pfx")` → `EXECUTOR_CANCEL_pfx`.
pub fn cancel_stream_name(prefix: Option<&str>) -> String {
    match prefix {
        Some(pfx) => format!("{CANCEL_STREAM}_{pfx}"),
        None => CANCEL_STREAM.to_string(),
    }
}

/// NATS subscription filter for the worker's cancel listener.
/// `None` → `executor.cancel.*`; `Some("pfx")` → `pfx.executor.cancel.*`.
pub fn cancel_subject_filter(prefix: Option<&str>) -> String {
    match prefix {
        Some(pfx) => format!("{pfx}.executor.cancel.*"),
        None => "executor.cancel.*".to_string(),
    }
}

/// Canonical, shared NATS subject-token sanitizer.
///
/// This is the single sanitizer used across the platform for building NATS
/// subject tokens (status/event subjects here; `mekhan.silent_drops.{kind}`
/// in mekhan-service, which re-uses this via the crate-root re-export). It is
/// **strict**: only ASCII alphanumerics plus `_` and `-` survive; every other
/// character — including `.` (which would otherwise introduce an extra subject
/// token), spaces, and the NATS wildcards `>`/`*` — collapses to `_`.
///
/// On all current callers the inputs are already in `[A-Za-z0-9_-]` (UUID-derived
/// execution_ids, static snake_case `kind` literals), so the output is byte-for-byte
/// identical to the previous lenient form. The strictness is purely defensive against
/// a future caller passing a dotted/whitespaced token.
pub fn sanitize_subject_token(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_states() {
        assert!(!ExecutionStatus::Accepted.is_terminal());
        assert!(!ExecutionStatus::Running.is_terminal());
        assert!(ExecutionStatus::Completed.is_terminal());
        assert!(ExecutionStatus::Failed.is_terminal());
        assert!(ExecutionStatus::Cancelled.is_terminal());
        assert!(ExecutionStatus::TimedOut.is_terminal());
    }

    #[test]
    fn as_str_roundtrip() {
        for status in [
            ExecutionStatus::Accepted,
            ExecutionStatus::Running,
            ExecutionStatus::Completed,
            ExecutionStatus::Failed,
            ExecutionStatus::Cancelled,
            ExecutionStatus::TimedOut,
        ] {
            let s = status.as_str();
            let deserialized: ExecutionStatus =
                serde_json::from_value(serde_json::Value::String(s.to_string())).unwrap();
            assert_eq!(status, deserialized);
        }
    }

    #[test]
    fn subject_sanitization() {
        assert_eq!(sanitize_subject_token("train-alpha-0"), "train-alpha-0");
        assert_eq!(sanitize_subject_token("has spaces"), "has_spaces");
        assert_eq!(sanitize_subject_token("a>b*c"), "a_b_c");
        // Strict form also collapses '.' (would otherwise add a subject token).
        assert_eq!(sanitize_subject_token("a.b"), "a_b");
    }

    #[test]
    fn status_update_subject_and_msg_id() {
        let update = StatusUpdate {
            execution_id: "train-alpha-0".into(),
            workspace_id: "ws-acme".into(),
            status: ExecutionStatus::Completed,
            detail: serde_json::Value::Null,
            metadata: Default::default(),
            source: "exec-1".into(),
            timestamp: Utc::now(),
        };
        assert_eq!(
            update.subject(),
            "executor.status.ws-acme.train-alpha-0.completed"
        );
        // msg_id stays ws-free: execution_id is globally unique, so the dedup
        // key needs no workspace segment.
        assert_eq!(update.msg_id(), "train-alpha-0-completed");
    }

    #[test]
    fn cancel_subjects() {
        assert_eq!(cancel_subject("exec-1"), "executor.cancel.exec-1");
        assert_eq!(cancel_subject_filter(None), "executor.cancel.*");
        assert_eq!(
            cancel_subject_filter(Some("tenant")),
            "tenant.executor.cancel.*"
        );
    }

    #[test]
    fn cancel_stream_names() {
        assert_eq!(cancel_stream_name(None), "EXECUTOR_CANCEL");
        assert_eq!(cancel_stream_name(Some("tenant")), "EXECUTOR_CANCEL_tenant");
    }
}
