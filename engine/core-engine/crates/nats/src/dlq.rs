//! Dead-letter queue for unprocessable NATS messages.
//!
//! Messages whose processing fails terminally (parse errors, business
//! rejections, exhausted internal retries) are wrapped in a [`DlqEntry`]
//! and published to `petri-dlq.{class}` instead of being silently dropped.
//! The `PETRI_DLQ` stream retains entries for 30 days so operators can
//! inspect and replay them (`nats stream view PETRI_DLQ`).
//!
//! The subjects use a `petri-dlq.` prefix (not `petri.dlq.`) because the
//! `PETRI_GLOBAL` stream captures `petri.>` and JetStream rejects streams
//! with overlapping subjects.

use base64::Engine as _;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::subjects::Subjects;

/// Classification of why a message was dead-lettered.
///
/// Mirrors the terminal [`crate::message_loop::ProcessError`] variants
/// (`Transient` never dead-letters — it is NACKed for redelivery).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DlqErrorClass {
    /// Payload (or subject) could not be parsed.
    Parse,
    /// Processing was rejected by domain logic (e.g. unknown place).
    Business,
    /// Internal error that did not resolve within the retry budget.
    Internal,
}

impl DlqErrorClass {
    /// Lowercase name used as the DLQ subject token.
    pub fn as_str(&self) -> &'static str {
        match self {
            DlqErrorClass::Parse => "parse",
            DlqErrorClass::Business => "business",
            DlqErrorClass::Internal => "internal",
        }
    }
}

/// Envelope published to `petri-dlq.{class}` for each dead-lettered message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlqEntry {
    /// Subject the original message was published to.
    pub original_subject: String,
    /// Why the message was dead-lettered.
    pub error_class: DlqErrorClass,
    /// Error message from the handler.
    pub error: String,
    /// Listener that failed to process the message.
    pub listener: String,
    /// JetStream delivery count at the time of dead-lettering.
    pub delivered: i64,
    /// When the entry was created.
    pub timestamp: DateTime<Utc>,
    /// Original payload, passed through verbatim when it is valid JSON
    /// (keeps the envelope inspectable via the nats CLI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// Base64-encoded original payload when it is not valid JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_base64: Option<String>,
}

impl DlqEntry {
    /// Build an entry from raw message parts. The payload travels as JSON
    /// when it parses, base64 otherwise.
    pub fn new(
        original_subject: impl Into<String>,
        error_class: DlqErrorClass,
        error: impl Into<String>,
        listener: impl Into<String>,
        delivered: i64,
        payload: &[u8],
    ) -> Self {
        let (json, b64) = match serde_json::from_slice::<serde_json::Value>(payload) {
            Ok(v) => (Some(v), None),
            Err(_) => (
                None,
                Some(base64::engine::general_purpose::STANDARD.encode(payload)),
            ),
        };
        Self {
            original_subject: original_subject.into(),
            error_class,
            error: error.into(),
            listener: listener.into(),
            delivered,
            timestamp: Utc::now(),
            payload: json,
            payload_base64: b64,
        }
    }
}

/// Returns the standard PETRI_DLQ stream configuration.
///
/// Companion to [`crate::stream_config`] — keeps stream creation idempotent
/// across the engine binary and listeners.
pub fn dlq_stream_config() -> async_nats::jetstream::stream::Config {
    use async_nats::jetstream::stream::{Config, RetentionPolicy, StorageType};
    use std::time::Duration;

    Config {
        name: Subjects::STREAM_DLQ.to_string(),
        subjects: vec![Subjects::DLQ_ALL.to_string()],
        retention: RetentionPolicy::Limits,
        storage: StorageType::File,
        max_age: Duration::from_secs(30 * 24 * 60 * 60), // 30 days
        ..Default::default()
    }
}

/// Publishes [`DlqEntry`]s to the `PETRI_DLQ` stream.
///
/// Cheap to clone — the JetStream context is Arc-wrapped internally.
#[derive(Clone)]
pub struct DlqPublisher {
    jetstream: async_nats::jetstream::Context,
}

impl DlqPublisher {
    pub fn new(jetstream: async_nats::jetstream::Context) -> Self {
        Self { jetstream }
    }

    /// Publish an entry and wait for the JetStream ack.
    ///
    /// Ensures the DLQ stream exists first (idempotent; dead letters are
    /// rare, so the extra round trip is acceptable) — dead-lettering works
    /// even if startup stream provisioning was skipped.
    pub async fn publish(&self, entry: &DlqEntry) -> Result<(), String> {
        self.jetstream
            .get_or_create_stream(dlq_stream_config())
            .await
            .map_err(|e| format!("ensure {} stream: {}", Subjects::STREAM_DLQ, e))?;
        let subject = Subjects::dlq_subject(entry.error_class.as_str());
        let payload = serde_json::to_vec(entry).map_err(|e| e.to_string())?;
        self.jetstream
            .publish(subject, payload.into())
            .await
            .map_err(|e| e.to_string())?
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_class_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&DlqErrorClass::Parse).unwrap(),
            "\"parse\""
        );
        assert_eq!(
            serde_json::to_string(&DlqErrorClass::Business).unwrap(),
            "\"business\""
        );
        assert_eq!(
            serde_json::to_string(&DlqErrorClass::Internal).unwrap(),
            "\"internal\""
        );
    }

    #[test]
    fn test_entry_json_payload_passthrough() {
        let entry = DlqEntry::new(
            "petri.signal.net-a.inbox",
            DlqErrorClass::Business,
            "place not found",
            "signal",
            1,
            br#"{"key": "value"}"#,
        );
        assert_eq!(entry.payload, Some(serde_json::json!({"key": "value"})));
        assert!(entry.payload_base64.is_none());

        let roundtrip: DlqEntry =
            serde_json::from_slice(&serde_json::to_vec(&entry).unwrap()).unwrap();
        assert_eq!(roundtrip.error_class, DlqErrorClass::Business);
        assert_eq!(roundtrip.original_subject, "petri.signal.net-a.inbox");
    }

    #[test]
    fn test_entry_non_json_payload_base64() {
        let entry = DlqEntry::new(
            "petri.commands.inject.token",
            DlqErrorClass::Parse,
            "invalid json",
            "token-injection",
            1,
            b"\xff\xfenot json",
        );
        assert!(entry.payload.is_none());
        let b64 = entry.payload_base64.expect("base64 payload");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        assert_eq!(decoded, b"\xff\xfenot json");
    }
}
