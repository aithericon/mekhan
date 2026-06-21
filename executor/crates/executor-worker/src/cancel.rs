use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_nats::jetstream;
use futures::StreamExt;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Shared registry mapping execution_id → CancellationToken.
///
/// Thread-safe via `Mutex<HashMap>`. Contention is minimal (register on job start,
/// deregister on job end, cancel is rare and point-lookup only), so a std Mutex
/// with trivially short critical sections is sufficient.
#[derive(Clone, Default)]
pub struct CancellationRegistry {
    inner: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl CancellationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new token for an execution. Returns the token to pass to the backend.
    ///
    /// If a token already existed for this execution_id, it is replaced.
    pub fn register(&self, execution_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        let mut map = self.inner.lock().unwrap();
        map.insert(execution_id.to_string(), token.clone());
        token
    }

    /// Deregister a token (called when execution finishes, regardless of outcome).
    pub fn deregister(&self, execution_id: &str) {
        let mut map = self.inner.lock().unwrap();
        map.remove(execution_id);
    }

    /// Cancel an execution by ID. Returns `true` if the execution was found and
    /// cancelled, `false` if not found (already finished or never existed).
    pub fn cancel(&self, execution_id: &str) -> bool {
        let map = self.inner.lock().unwrap();
        if let Some(token) = map.get(execution_id) {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Number of currently active (registered) executions.
    pub fn active_count(&self) -> usize {
        self.inner.lock().unwrap().len()
    }
}

/// Listens on the JetStream `EXECUTOR_CANCEL` stream (`executor.cancel.*`) and
/// triggers cancellation via the registry.
///
/// Cancels ride JetStream rather than core NATS so the signal survives the
/// internal-NATS ↔ WebSocket-front-door boundary (core pub/sub interest never
/// propagated to WS-connected runners, so the old `client.subscribe` saw zero
/// `executor.cancel.*` messages in prod — see
/// [`aithericon_executor_domain::cancel_subject`]).
///
/// Each runner binds its **own ephemeral** pull consumer with
/// `DeliverPolicy::New`, so:
///   - every runner sees every cancel and ignores the ones it doesn't own
///     (fan-out — the stream is `Limits` retention, NOT `WorkQueue`), and
///   - a runner that (re)connects never replays a stale cancel onto a reused
///     execution id (delivery starts at "now").
///
/// Like the old core-NATS listener, cancellation is still effectively ephemeral:
/// a cancel published while the runner is fully down is not redelivered on
/// restart (fresh `New` consumer) — correct, since that execution isn't running
/// here anyway.
/// Timing knobs for the cancel listener's consumer lifecycle.
///
/// Production uses [`CancelListenerTuning::default`] (15s heartbeat, 5min
/// inactive-reap backstop, 1s rebind backoff). Tests shrink these so the
/// idle-survival and rebind-on-dead-consumer behaviours are observable in
/// seconds rather than minutes.
#[derive(Clone, Copy, Debug)]
pub struct CancelListenerTuning {
    /// Ephemeral consumer idle-reap window (`inactive_threshold`). The continuous
    /// heartbeated pull keeps the consumer active, so this backstop is
    /// effectively never hit in normal operation.
    pub inactive_threshold: Duration,
    /// Heartbeat interval on the message stream. Surfaces a silently-stalled or
    /// reaped consumer as an error so the listener rebinds instead of hanging,
    /// and the continuous pull it drives keeps the consumer's inactivity timer
    /// reset.
    pub heartbeat: Duration,
    /// Backoff before rebinding a fresh consumer after a stream error/close.
    pub rebind_backoff: Duration,
}

impl Default for CancelListenerTuning {
    fn default() -> Self {
        Self {
            inactive_threshold: Duration::from_secs(300),
            heartbeat: Duration::from_secs(15),
            rebind_backoff: Duration::from_secs(1),
        }
    }
}

/// Handle to a running cancel listener: its task plus an observable rebind
/// counter.
///
/// `rebinds` is the number of times the listener replaced a reaped/errored
/// consumer with a fresh one — 0 in steady state. Tests assert on it to
/// distinguish "the heartbeat kept the original consumer alive" (idle-survival,
/// `rebinds == 0`) from "the consumer died and the listener recovered"
/// (rebind-on-dead-consumer, `rebinds >= 1`).
pub struct CancelListenerHandle {
    pub handle: JoinHandle<()>,
    pub rebinds: Arc<AtomicU64>,
}

pub struct NatsCancelListener;

impl NatsCancelListener {
    /// Start listening for cancel messages. Returns a `JoinHandle` for the listener task.
    ///
    /// `prefix` follows the same convention as `StatusReporter.subject_prefix`:
    ///   - `None`  → stream `EXECUTOR_CANCEL`, filter `executor.cancel.*`
    ///   - `Some("pfx")` → stream `EXECUTOR_CANCEL_pfx`, filter `pfx.executor.cancel.*`
    ///
    /// Ensures the `EXECUTOR_CANCEL` stream exists (idempotent `get_or_create`)
    /// before binding, so the runner can start before any publisher.
    ///
    /// Thin wrapper over [`Self::start_with_tuning`] with production timings;
    /// discards the rebind counter and returns just the task handle.
    pub async fn start(
        jetstream: jetstream::Context,
        registry: CancellationRegistry,
        prefix: Option<&str>,
        replicas: usize,
        shutdown: CancellationToken,
    ) -> Result<JoinHandle<()>, async_nats::Error> {
        Ok(Self::start_with_tuning(
            jetstream,
            registry,
            prefix,
            replicas,
            shutdown,
            CancelListenerTuning::default(),
        )
        .await?
        .handle)
    }

    /// Like [`Self::start`] but with caller-supplied [`CancelListenerTuning`] and
    /// a [`CancelListenerHandle`] exposing the rebind counter — used by tests to
    /// drive the consumer lifecycle deterministically.
    pub async fn start_with_tuning(
        jetstream: jetstream::Context,
        registry: CancellationRegistry,
        prefix: Option<&str>,
        replicas: usize,
        shutdown: CancellationToken,
        tuning: CancelListenerTuning,
    ) -> Result<CancelListenerHandle, async_nats::Error> {
        let stream_name = aithericon_executor_domain::cancel_stream_name(prefix);
        let filter = aithericon_executor_domain::cancel_subject_filter(prefix);

        // Idempotently ensure the cancel stream. Transient signal: short max-age,
        // `Limits` retention (every runner's consumer reads the same messages —
        // a `WorkQueue` would hand each cancel to exactly one runner), discard
        // oldest under pressure.
        jetstream
            .get_or_create_stream(jetstream::stream::Config {
                name: stream_name.clone(),
                subjects: vec![filter.clone()],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_age: Duration::from_secs(
                    aithericon_executor_domain::CANCEL_STREAM_MAX_AGE_SECS,
                ),
                storage: jetstream::stream::StorageType::File,
                num_replicas: replicas,
                discard: jetstream::stream::DiscardPolicy::Old,
                ..Default::default()
            })
            .await?;

        // Bind the first consumer synchronously, BEFORE returning, so a caller
        // that publishes a cancel immediately after `start()` is not lost: the
        // `DeliverPolicy::New` consumer only sees messages published after it
        // exists, so the bind must precede any publish the caller sequences after
        // us (the integration test relies on this; prod is naturally racey-safe
        // since a job must already be running to be cancelled).
        let mut consumer =
            Self::bind_consumer(&jetstream, &stream_name, &filter, tuning.inactive_threshold)
                .await?;

        info!(%stream_name, %filter, "JetStream cancel listener started");

        let rebinds = Arc::new(AtomicU64::new(0));
        let rebinds_task = rebinds.clone();

        let handle = tokio::spawn(async move {
            'outer: loop {
                // Open a heartbeated message stream on the current consumer. The
                // heartbeat surfaces a silently-stalled delivery as an error so we
                // rebind instead of hanging; the continuous pull also keeps the
                // ephemeral consumer's inactivity timer reset (so it is not reaped
                // between cancels, which can be hours apart).
                let mut messages = match consumer
                    .stream()
                    .heartbeat(tuning.heartbeat)
                    .messages()
                    .await
                {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(error = %e, "failed to open cancel message stream");
                        if !Self::rebind(
                            &jetstream,
                            &stream_name,
                            &filter,
                            &shutdown,
                            &mut consumer,
                            tuning,
                            &rebinds_task,
                        )
                        .await
                        {
                            break 'outer;
                        }
                        continue 'outer;
                    }
                };

                loop {
                    tokio::select! {
                        biased;
                        _ = shutdown.cancelled() => {
                            info!("JetStream cancel listener shutting down");
                            break 'outer;
                        }
                        next = messages.next() => {
                            match next {
                                Some(Ok(msg)) => {
                                    if let Some(execution_id) =
                                        msg.subject.as_str().split('.').next_back()
                                    {
                                        let found = registry.cancel(execution_id);
                                        if found {
                                            info!(%execution_id, "cancellation triggered via JetStream");
                                        } else {
                                            debug!(
                                                %execution_id,
                                                "cancel request for unknown execution (already finished?)"
                                            );
                                        }
                                    }
                                    // AckPolicy::None — nothing to ack.
                                }
                                // A deleted/stalled ephemeral consumer surfaces here
                                // (e.g. "no responders"). Break to rebind a FRESH
                                // consumer rather than reusing the dead one (which
                                // would tight-loop on the same error).
                                Some(Err(e)) => {
                                    warn!(error = %e, "cancel message error; rebinding consumer");
                                    break;
                                }
                                None => {
                                    warn!("cancel message stream closed; rebinding consumer");
                                    break;
                                }
                            }
                        }
                    }
                }

                // Inner loop exited on a stream error/close: back off, then replace
                // the consumer with a fresh bind before looping.
                if !Self::rebind(
                    &jetstream,
                    &stream_name,
                    &filter,
                    &shutdown,
                    &mut consumer,
                    tuning,
                    &rebinds_task,
                )
                .await
                {
                    break 'outer;
                }
            }
        });

        Ok(CancelListenerHandle { handle, rebinds })
    }

    /// Back off (interruptible by shutdown), then replace `consumer` with a fresh
    /// ephemeral bind. Returns `false` if shutdown fired during the backoff (the
    /// caller should stop). A failed rebind leaves the stale consumer in place;
    /// the next `messages()` call errors and routes back here, so the backoff
    /// bounds the retry rate. Increments `rebinds` on each successful rebind.
    async fn rebind(
        jetstream: &jetstream::Context,
        stream_name: &str,
        filter: &str,
        shutdown: &CancellationToken,
        consumer: &mut jetstream::consumer::PullConsumer,
        tuning: CancelListenerTuning,
        rebinds: &AtomicU64,
    ) -> bool {
        if Self::sleep_or_shutdown(shutdown, tuning.rebind_backoff).await {
            return false;
        }
        match Self::bind_consumer(jetstream, stream_name, filter, tuning.inactive_threshold).await {
            Ok(c) => {
                *consumer = c;
                rebinds.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => warn!(error = %e, "failed to rebind cancel consumer; will retry"),
        }
        true
    }

    /// Bind a fresh ephemeral pull consumer (no `durable_name`) on the cancel
    /// stream. `DeliverPolicy::New` + `AckPolicy::None`: deliver only cancels
    /// published after this bind, fire-and-forget (no redelivery).
    ///
    /// `inactive_threshold` is a backstop only — the continuous pull keeps the
    /// consumer active, so this idle-reap window is effectively never hit during
    /// normal operation (generous in prod so a brief rebind gap can't
    /// orphan-reap it; tiny in tests to exercise the reap path).
    async fn bind_consumer(
        jetstream: &jetstream::Context,
        stream_name: &str,
        filter: &str,
        inactive_threshold: Duration,
    ) -> Result<jetstream::consumer::PullConsumer, async_nats::Error> {
        let stream = jetstream.get_stream(stream_name).await?;
        let consumer = stream
            .create_consumer(jetstream::consumer::pull::Config {
                filter_subject: filter.to_string(),
                deliver_policy: jetstream::consumer::DeliverPolicy::New,
                ack_policy: jetstream::consumer::AckPolicy::None,
                inactive_threshold,
                ..Default::default()
            })
            .await?;
        Ok(consumer)
    }

    /// Sleep for `dur`, or return `true` early if shutdown fired.
    async fn sleep_or_shutdown(shutdown: &CancellationToken, dur: Duration) -> bool {
        tokio::select! {
            _ = shutdown.cancelled() => true,
            _ = tokio::time::sleep(dur) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_cancel() {
        let registry = CancellationRegistry::new();
        let token = registry.register("exec-1");
        assert!(!token.is_cancelled());
        assert_eq!(registry.active_count(), 1);

        let found = registry.cancel("exec-1");
        assert!(found);
        assert!(token.is_cancelled());
    }

    #[test]
    fn cancel_unknown_is_noop() {
        let registry = CancellationRegistry::new();
        let found = registry.cancel("nonexistent");
        assert!(!found);
    }

    #[test]
    fn deregister_removes_token() {
        let registry = CancellationRegistry::new();
        let _token = registry.register("exec-1");
        assert_eq!(registry.active_count(), 1);

        registry.deregister("exec-1");
        assert_eq!(registry.active_count(), 0);

        let found = registry.cancel("exec-1");
        assert!(!found);
    }

    #[test]
    fn register_replaces_existing() {
        let registry = CancellationRegistry::new();
        let token1 = registry.register("exec-1");
        let token2 = registry.register("exec-1");
        assert_eq!(registry.active_count(), 1);

        // Cancelling should affect the new token
        registry.cancel("exec-1");
        assert!(token2.is_cancelled());
        // Old token is no longer tracked, but was not cancelled via registry
        assert!(!token1.is_cancelled());
    }
}
