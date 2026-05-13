use crate::{NatsContext, NatsStorage};
use apalis_core::backend::{BackendExpose, Stat, WorkerState};
use apalis_core::error::Error;
use apalis_core::request::{Request, State};
use apalis_core::service_fn::FromRequest;
use apalis_core::worker::Worker;
use async_nats::jetstream::{self, consumer};
use serde::{de::DeserializeOwned, Serialize};
use std::sync::Arc;
use std::time::Duration;

impl<T> BackendExpose<Request<T, NatsContext>> for NatsStorage<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    type Request = Request<T, NatsContext>;
    type Error = Error;

    async fn stats(&self) -> Result<Stat, Self::Error> {
        let mut pending = 0usize;
        let mut failed = 0usize;
        let success = 0usize;

        // Collect stats from all priority streams
        for priority in [
            crate::Priority::High,
            crate::Priority::Medium,
            crate::Priority::Low,
        ] {
            let stream_name = self.get_stream_name(priority);
            if let Ok(mut stream) = self.jetstream.get_stream(stream_name).await {
                if let Ok(info) = stream.info().await {
                    pending += info.state.messages as usize;
                    // Note: NATS doesn't directly track success/failure counts
                    // These would need to be tracked separately if needed
                }
            }
        }

        // Check DLQ for failed jobs
        if self.config.enable_dlq {
            let dlq_stream_name = format!("{}_dlq", self.config.namespace);
            if let Ok(mut stream) = self.jetstream.get_stream(dlq_stream_name).await {
                if let Ok(info) = stream.info().await {
                    failed = info.state.messages as usize;
                }
            }
        }

        Ok(Stat {
            pending,
            running: 0, // NATS doesn't track running state
            dead: 0,
            failed,
            success,
        })
    }

    async fn list_jobs(
        &self,
        _status: &State,
        _page: i32,
    ) -> Result<Vec<Self::Request>, Self::Error> {
        // NATS streams don't support listing in the traditional sense
        // This would require implementing a separate index or using NATS KV
        Ok(vec![])
    }

    async fn list_workers(&self) -> Result<Vec<Worker<WorkerState>>, Self::Error> {
        // With shared consumers, we don't track individual workers through NATS
        // Instead, we return info about the shared consumers
        let mut workers = Vec::new();

        for priority in [
            crate::Priority::High,
            crate::Priority::Medium,
            crate::Priority::Low,
        ] {
            let consumer_name = format!("{}_{}_consumer", self.config.namespace, priority);
            let stream_name = self.get_stream_name(priority);

            // Check if the shared consumer exists
            if let Ok(stream) = self.jetstream.get_stream(stream_name).await {
                if let Ok(_consumer) = stream
                    .get_consumer::<consumer::pull::Config>(&consumer_name)
                    .await
                {
                    let worker_id = apalis_core::worker::WorkerId::new(consumer_name.clone());
                    let worker = Worker::new(worker_id, WorkerState::new::<Self>(consumer_name));
                    workers.push(worker);
                }
            }
        }

        Ok(workers)
    }
}

impl NatsContext {
    /// Acknowledge the message as successfully processed
    pub async fn ack(&self) -> Result<(), Error> {
        if let Some(msg) = &self.message {
            msg.ack().await.map_err(|e| Error::SourceError(Arc::new(e)))
        } else {
            Ok(())
        }
    }

    /// Negatively acknowledge the message for retry
    pub async fn nack(&self) -> Result<(), Error> {
        if let Some(msg) = &self.message {
            msg.ack_with(jetstream::AckKind::Nak(None))
                .await
                .map_err(|e| Error::SourceError(Arc::new(e)))
        } else {
            Ok(())
        }
    }

    /// Terminate processing (send to DLQ if configured)
    pub async fn term(&self) -> Result<(), Error> {
        if let Some(msg) = &self.message {
            msg.ack_with(jetstream::AckKind::Term)
                .await
                .map_err(|e| Error::SourceError(Arc::new(e)))
        } else {
            Ok(())
        }
    }

    /// Request progress update (extends ack wait time)
    pub async fn progress(&self) -> Result<(), Error> {
        if let Some(msg) = &self.message {
            msg.ack_with(jetstream::AckKind::Progress)
                .await
                .map_err(|e| Error::SourceError(Arc::new(e)))
        } else {
            Ok(())
        }
    }
}

/// A guard that periodically sends Progress acknowledgements to extend ack wait.
/// Drop to stop heartbeating.
#[derive(Debug)]
pub struct ProgressGuard {
    #[allow(dead_code)]
    handle: tokio::task::JoinHandle<()>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Drop for ProgressGuard {
    fn drop(&mut self) {
        // Best-effort stop signal; task may already be completed
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // We intentionally don't await here
    }
}

impl NatsContext {
    /// Start a background heartbeat that periodically calls Progress to extend ack wait.
    /// Returns None if no underlying message is present (e.g., synthetic requests).
    pub fn start_progress_heartbeat(&self, interval: Duration) -> Option<ProgressGuard> {
        let msg = self.message.as_ref()?.clone();
        let (tx, mut rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = &mut rx => break,
                    _ = ticker.tick() => {
                        let _ = msg.ack_with(jetstream::AckKind::Progress).await;
                    }
                }
            }
        });
        Some(ProgressGuard {
            handle,
            stop: Some(tx),
        })
    }
}

impl<Req> FromRequest<Request<Req, NatsContext>> for NatsContext {
    fn from_request(req: &Request<Req, NatsContext>) -> Result<Self, Error> {
        Ok(req.parts.context.clone())
    }
}
