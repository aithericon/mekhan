use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use futures::StreamExt;

use crate::nats::MekhanNats;
use crate::tasks::process_types::{ProcessState, ProcessUpdate, ProcessUpdateType};

/// In-memory index of process states, projected from NATS HUMAN_PROCESS stream.
#[derive(Clone)]
pub struct ProcessIndex {
    processes: Arc<DashMap<String, ProcessState>>,
}

impl ProcessIndex {
    pub fn new() -> Self {
        Self {
            processes: Arc::new(DashMap::new()),
        }
    }

    /// List all processes, optionally filtered by status.
    pub fn list(&self, status: Option<&str>) -> Vec<ProcessState> {
        self.processes
            .iter()
            .filter(|entry| {
                status.map_or(true, |s| entry.value().status == s)
            })
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Get a single process by ID.
    pub fn get(&self, process_id: &str) -> Option<ProcessState> {
        self.processes.get(process_id).map(|e| e.value().clone())
    }

    /// Apply a process update to the index.
    fn apply_update(&self, update: &ProcessUpdate) {

        match &update.update_type {
            ProcessUpdateType::Started { metadata } => {
                let state = ProcessState::from_metadata(metadata);
                self.processes.insert(update.process_id.clone(), state);
            }
            _ => {
                if let Some(mut entry) = self.processes.get_mut(&update.process_id) {
                    entry.value_mut().apply(update);
                } else {
                    tracing::debug!(
                        process_id = %update.process_id,
                        "Process update for unknown process (may have been started before service start)"
                    );
                }
            }
        }
    }

    /// Start a background consumer that reads from the HUMAN_PROCESS stream.
    pub async fn start_consumer(self, nats: MekhanNats) {
        let js = nats.jetstream().clone();

        // Ensure the stream exists (it may not if no processes have been started yet)
        let stream = match js.get_stream("HUMAN_PROCESS").await {
            Ok(s) => s,
            Err(e) => {
                tracing::info!("HUMAN_PROCESS stream not found ({e}), process tracking disabled until first process starts");
                // Retry periodically
                let index = self.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        match js.get_stream("HUMAN_PROCESS").await {
                            Ok(s) => {
                                tracing::info!("HUMAN_PROCESS stream found, starting consumer");
                                index.consume_stream(s).await;
                                return;
                            }
                            Err(_) => continue,
                        }
                    }
                });
                return;
            }
        };

        self.consume_stream(stream).await;
    }

    async fn consume_stream(&self, stream: async_nats::jetstream::stream::Stream) {
        let consumer = match stream
            .get_or_create_consumer(
                "mekhan-process-updates",
                async_nats::jetstream::consumer::pull::Config {
                    durable_name: Some("mekhan-process-updates".into()),
                    filter_subject: "human.process.>".into(),
                    ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                    deliver_policy: async_nats::jetstream::consumer::DeliverPolicy::All,
                    ..Default::default()
                },
            )
            .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Failed to create process consumer: {e}");
                return;
            }
        };

        let mut messages = match consumer.stream().heartbeat(std::time::Duration::ZERO).messages().await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("Failed to get process consumer messages: {e}");
                return;
            }
        };

        tracing::info!("Process index consumer started");

        let mut count = 0u64;
        while let Some(result) = messages.next().await {
            match result {
                Ok(msg) => {
                    count += 1;
                    match serde_json::from_slice::<ProcessUpdate>(&msg.payload) {
                        Ok(update) => {
                            tracing::debug!(
                                process_id = %update.process_id,
                                update_type = ?std::mem::discriminant(&update.update_type),
                                count,
                                "Applied process update"
                            );
                            self.apply_update(&update);
                        }
                        Err(e) => {
                            let preview = String::from_utf8_lossy(&msg.payload[..msg.payload.len().min(200)]);
                            tracing::warn!(%e, %preview, "Failed to parse process update");
                        }
                    }
                    if let Err(e) = msg.ack().await {
                        tracing::warn!("Failed to ack process message: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("Process consumer message error: {e}");
                }
            }
        }

        tracing::warn!(count, "Process index consumer stream ended");
    }
}
