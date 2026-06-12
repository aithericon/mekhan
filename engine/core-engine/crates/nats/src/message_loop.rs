//! Generic NATS JetStream message loop.
//!
//! Extracts the common consume-parse-process-ack pattern shared by all
//! listeners into a single [`run_message_loop`] driver function.
//! Each listener implements [`MessageHandler`] to supply only its
//! parsing and processing logic.

use std::time::Duration;

use async_nats::jetstream::consumer::PullConsumer;
use async_nats::jetstream::Message;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::dlq::{DlqEntry, DlqErrorClass, DlqPublisher};

/// Deliveries allowed for `ProcessError::Internal` before dead-lettering.
const INTERNAL_MAX_DELIVERIES: i64 = 5;

/// Outcome of pre-processing a raw NATS message before deserialization.
pub enum PreProcessResult {
    /// Continue to deserialization and processing.
    Continue,
    /// ACK and skip this message (e.g., stale epoch).
    Skip,
}

/// Errors from [`run_message_loop`].
#[derive(Debug, thiserror::Error)]
pub enum MessageLoopError {
    #[error("Consumer error: {0}")]
    Consumer(String),
}

/// Errors returned by [`MessageHandler::process_message`].
///
/// The variant controls how the message loop disposes of the message:
/// - `Parse` — WARN (includes the raw payload), dead-letter + ACK
/// - `Business` — WARN, dead-letter + ACK
/// - `Internal` — ERROR, NACK with escalating delay up to
///   [`INTERNAL_MAX_DELIVERIES`] deliveries, then dead-letter + ACK
/// - `Transient` — WARN, NACK + redeliver (never dead-lettered)
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("{0}")]
    Parse(String),

    #[error("{0}")]
    Business(String),

    #[error("{0}")]
    Internal(String),

    /// Transient failure — NACK the message for redelivery instead of ACKing.
    /// Used when the target is temporarily unavailable (e.g., child net not yet created).
    #[error("{0}")]
    Transient(String),
}

/// Trait that each listener implements to plug into the shared message loop.
///
/// `process_message` receives the raw NATS [`Message`] and is responsible
/// for its own deserialization. This is necessary because several listeners
/// extract data from the NATS subject before touching the payload, and
/// others inspect message metadata for idempotency.
#[async_trait::async_trait]
pub trait MessageHandler: Send + Sync {
    /// Human-readable name for log messages (e.g., "token-injection").
    fn listener_name(&self) -> &str;

    /// Optional pre-processing hook called before `process_message`.
    ///
    /// Override to implement epoch checks, header inspection, etc.
    fn pre_process(&self, _msg: &Message) -> PreProcessResult {
        PreProcessResult::Continue
    }

    /// Parse and process a single NATS message.
    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError>;
}

/// Run the consume-process-ack loop for any [`MessageHandler`].
///
/// This is the single place where NATS listener boilerplate lives.
/// If `cancel` is provided, the loop exits when the token is cancelled.
pub async fn run_message_loop<H: MessageHandler>(
    consumer: PullConsumer,
    handler: &H,
) -> Result<(), MessageLoopError> {
    run_message_loop_cancellable(consumer, handler, None, None).await
}

/// Run the consume-process-ack loop with optional cancellation support.
///
/// When `dlq` is provided, terminally-failed messages (Parse/Business, or
/// Internal after the retry budget) are published as [`DlqEntry`]s before
/// being ACKed. Without it, they are ACKed and dropped (legacy behavior —
/// only acceptable for tests).
pub async fn run_message_loop_cancellable<H: MessageHandler>(
    consumer: PullConsumer,
    handler: &H,
    cancel: Option<CancellationToken>,
    dlq: Option<DlqPublisher>,
) -> Result<(), MessageLoopError> {
    let name = handler.listener_name();

    tracing::info!(listener = %name, "Listener started");

    // Consumer idle heartbeat detects stalled delivery. With ping_interval
    // keeping the TCP connection alive (see NatsConfig), the heartbeat
    // won't fire spuriously. On missed heartbeat the stream returns an
    // error; the loop continues and the pull consumer self-heals on the
    // next batch request.
    let mut messages = consumer
        .stream()
        .heartbeat(Duration::from_secs(15))
        .messages()
        .await
        .map_err(|e| {
            MessageLoopError::Consumer(format!("{}: failed to get message stream: {}", name, e))
        })?;

    loop {
        let msg_result = if let Some(ref token) = cancel {
            tokio::select! {
                _ = token.cancelled() => {
                    tracing::info!(listener = %name, "Listener cancelled");
                    break;
                }
                msg = messages.next() => match msg {
                    Some(r) => r,
                    None => break,
                }
            }
        } else {
            match messages.next().await {
                Some(r) => r,
                None => break,
            }
        };
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(listener = %name, error = %e, "Error receiving NATS message");
                continue;
            }
        };

        // Pre-processing hook (epoch check, etc.)
        match handler.pre_process(&msg) {
            PreProcessResult::Continue => {}
            PreProcessResult::Skip => {
                let _ = msg.ack().await;
                continue;
            }
        }

        // Delegate to handler
        match handler.process_message(&msg).await {
            Ok(()) => {}
            Err(ProcessError::Transient(ref e)) => {
                // Transient error — NACK for redelivery (e.g., target net not yet created)
                tracing::warn!(listener = %name, error = %e, "Transient error, NACKing for redelivery");
                if let Err(e) = msg
                    .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                        Duration::from_millis(500),
                    )))
                    .await
                {
                    tracing::error!(listener = %name, error = %e, "Failed to NACK message");
                }
                continue;
            }
            Err(ProcessError::Parse(ref e)) => {
                tracing::warn!(
                    listener = %name,
                    error = %e,
                    payload = %String::from_utf8_lossy(&msg.payload),
                    "Failed to parse message, dead-lettering"
                );
                if !dead_letter(&dlq, &msg, DlqErrorClass::Parse, e, name).await {
                    continue;
                }
            }
            Err(ProcessError::Business(ref e)) => {
                tracing::warn!(listener = %name, error = %e, "Processing failed, dead-lettering");
                if !dead_letter(&dlq, &msg, DlqErrorClass::Business, e, name).await {
                    continue;
                }
            }
            Err(ProcessError::Internal(ref e)) => {
                // Internal errors are retried with escalating backoff before
                // being dead-lettered — unlike Parse/Business, a retry can
                // plausibly succeed (e.g. a dependency hiccup).
                let delivered = delivery_count(&msg);
                if delivered < INTERNAL_MAX_DELIVERIES {
                    tracing::error!(
                        listener = %name,
                        error = %e,
                        delivered,
                        "Internal error, NACKing for retry"
                    );
                    let delay = Duration::from_millis(500 * delivered.max(1) as u64);
                    if let Err(e) = msg
                        .ack_with(async_nats::jetstream::AckKind::Nak(Some(delay)))
                        .await
                    {
                        tracing::error!(listener = %name, error = %e, "Failed to NACK message");
                    }
                    continue;
                }
                tracing::error!(
                    listener = %name,
                    error = %e,
                    delivered,
                    "Internal error, retry budget exhausted — dead-lettering"
                );
                if !dead_letter(&dlq, &msg, DlqErrorClass::Internal, e, name).await {
                    continue;
                }
            }
        }

        // ACK (reached for Ok + dead-lettered Parse/Business/Internal;
        // Transient and retried/NAK'd paths continue above)
        if let Err(e) = msg.ack().await {
            tracing::error!(listener = %name, error = %e, "Failed to ACK message");
        }
    }

    tracing::info!(listener = %name, "Listener stopped");
    Ok(())
}

/// JetStream delivery count for a message (1 on first delivery).
fn delivery_count(msg: &Message) -> i64 {
    msg.info().map(|i| i.delivered).unwrap_or(1)
}

/// Publish a [`DlqEntry`] for a terminally-failed message.
///
/// Returns `true` if the caller may ACK the message (entry persisted, or no
/// DLQ publisher configured). On publish failure the message is NACKed with
/// a delay — redelivered rather than lost — and `false` is returned.
async fn dead_letter(
    dlq: &Option<DlqPublisher>,
    msg: &Message,
    class: DlqErrorClass,
    error: &str,
    listener: &str,
) -> bool {
    let Some(publisher) = dlq else {
        return true;
    };
    let entry = DlqEntry::new(
        msg.subject.as_str(),
        class,
        error,
        listener,
        delivery_count(msg),
        &msg.payload,
    );
    match publisher.publish(&entry).await {
        Ok(()) => true,
        Err(e) => {
            tracing::error!(
                listener = %listener,
                error = %e,
                class = %class.as_str(),
                "Failed to publish DLQ entry; NACKing original message"
            );
            if let Err(e) = msg
                .ack_with(async_nats::jetstream::AckKind::Nak(Some(
                    Duration::from_millis(500),
                )))
                .await
            {
                tracing::error!(
                    listener = %listener,
                    error = %e,
                    "Failed to NACK message after DLQ publish failure"
                );
            }
            false
        }
    }
}
