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
/// The variant controls the log level used by the message loop:
/// - `Parse` — WARN, includes the raw payload for debugging
/// - `Business` — WARN
/// - `Internal` — ERROR
/// - `Transient` — WARN, NACK + redeliver (message not lost)
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
    run_message_loop_cancellable(consumer, handler, None).await
}

/// Run the consume-process-ack loop with optional cancellation support.
pub async fn run_message_loop_cancellable<H: MessageHandler>(
    consumer: PullConsumer,
    handler: &H,
    cancel: Option<CancellationToken>,
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
                    "Failed to parse message"
                );
            }
            Err(ProcessError::Business(ref e)) => {
                tracing::warn!(listener = %name, error = %e, "Processing failed");
            }
            Err(ProcessError::Internal(ref e)) => {
                tracing::error!(listener = %name, error = %e, "Internal error");
            }
        }

        // ACK (reached for Ok + Parse/Business/Internal; Transient continues above)
        if let Err(e) = msg.ack().await {
            tracing::error!(listener = %name, error = %e, "Failed to ACK message");
        }
    }

    tracing::info!(listener = %name, "Listener stopped");
    Ok(())
}
