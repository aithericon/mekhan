//! SignalListener: receives external system signals via NATS and injects tokens.
//!
//! Subscribes to `petri.{ws}.{net_id}.signal.>` and injects tokens into the
//! target place when an `ExternalSignal` is received. Uses [`MessageHandler`]
//! and [`run_message_loop`] for the consume-parse-process-ack loop.
//!
//! Per ADR-09 the filter and durable name are workspace-segmented: a single
//! engine process hosts nets from many workspaces, so `ws` is threaded in at
//! construction and NEVER read from a process-global. The durable name was
//! already net-unique; it now carries the `{ws}` segment too so two workspaces
//! that ever shared a net id can't collide on the consumer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_nats::jetstream::Message;
use petri_application::{
    json_to_token_color, EventRepository, EventStoreError, PetriNetService, ServiceError,
    StateProjection, TopologyRepository,
};
use petri_domain::ExternalSignal;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::message_loop::{
    run_message_loop_cancellable, MessageHandler, PreProcessResult, ProcessError,
};
use crate::subjects::Subjects;

/// Errors from the signal listener.
#[derive(Debug, thiserror::Error)]
pub enum SignalListenerError {
    /// JetStream stream error.
    #[error("JetStream error: {0}")]
    JetStream(String),

    /// Consumer error.
    #[error("Consumer error: {0}")]
    Consumer(String),
}

/// Listener that receives external signals via NATS and injects tokens.
///
/// One instance per net. Subscribes to `petri.{ws}.{net_id}.signal.>` via a
/// JetStream pull consumer and injects tokens into the resolved place.
///
/// Uses an epoch (NATS stream sequence number) to filter stale messages
/// from previous scenario instances without deleting the consumer.
pub struct SignalListener {
    /// Workspace (tenant) this net belongs to. Threaded in at load time from
    /// `LoadScenarioRequest::workspace()`; defaults to
    /// [`Subjects::DEFAULT_WORKSPACE`] for legacy/SDK/demo loads. NEVER a
    /// process-global — that would collapse tenant isolation.
    workspace_id: String,
    net_id: String,
    jetstream: async_nats::jetstream::Context,
    /// Stream sequence number at or below which messages are stale.
    /// Set to 0 at startup (accept all), advanced on scenario reload.
    epoch: Arc<AtomicU64>,
}

impl SignalListener {
    /// Create a new signal listener for a specific net within a workspace.
    pub fn new(
        workspace_id: String,
        net_id: String,
        jetstream: async_nats::jetstream::Context,
    ) -> Self {
        Self {
            workspace_id,
            net_id,
            jetstream,
            epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Advance the epoch to the current stream tail.
    ///
    /// After this call, any messages with `stream_sequence <= epoch` are
    /// ACK'd without processing. Call on **scenario reload** to discard
    /// stale signals from a previous scenario instance while preserving
    /// crash recovery (engine restart without reload keeps epoch at 0,
    /// so all pending messages are processed).
    pub async fn advance_epoch(&self) -> Result<(), SignalListenerError> {
        let mut stream = self
            .jetstream
            .get_or_create_stream(crate::stream_config())
            .await
            .map_err(|e| SignalListenerError::JetStream(e.to_string()))?;
        let info = stream
            .info()
            .await
            .map_err(|e| SignalListenerError::JetStream(e.to_string()))?;
        let new_epoch = info.state.last_sequence;
        let old = self.epoch.swap(new_epoch, Ordering::SeqCst);
        tracing::info!(
            workspace_id = %self.workspace_id,
            net_id = %self.net_id,
            old_epoch = old,
            new_epoch = new_epoch,
            "Advanced signal listener epoch for scenario reload"
        );
        Ok(())
    }

    /// Start the signal listener as a spawned tokio task.
    ///
    /// Returns a `JoinHandle` for the listener task.
    pub fn start<E, T, S>(
        self: Arc<Self>,
        service: Arc<PetriNetService<E, T, S>>,
        eval_notify: Arc<Notify>,
    ) -> tokio::task::JoinHandle<()>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        self.start_cancellable(service, eval_notify, None)
    }

    /// Start the signal listener with optional cancellation support.
    pub fn start_cancellable<E, T, S>(
        self: Arc<Self>,
        service: Arc<PetriNetService<E, T, S>>,
        eval_notify: Arc<Notify>,
        cancel: Option<CancellationToken>,
    ) -> tokio::task::JoinHandle<()>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let listener = self.clone();
        tokio::spawn(async move {
            if let Err(e) = listener.run(service, eval_notify, cancel).await {
                tracing::error!(
                    error = %e,
                    net_id = %listener.net_id,
                    "Signal listener stopped with error"
                );
            }
        })
    }

    /// Run the listener loop.
    async fn run<E, T, S>(
        &self,
        service: Arc<PetriNetService<E, T, S>>,
        eval_notify: Arc<Notify>,
        cancel: Option<CancellationToken>,
    ) -> Result<(), SignalListenerError>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        use async_nats::jetstream::consumer::{
            pull::Config as ConsumerConfig, AckPolicy, DeliverPolicy,
        };

        let stream = self
            .jetstream
            .get_or_create_stream(crate::stream_config())
            .await
            .map_err(|e| SignalListenerError::JetStream(e.to_string()))?;
        let filter = Subjects::signal_inbox_filter(&self.workspace_id, &self.net_id);
        // Durable name carries the workspace segment so two workspaces can
        // never collide on the consumer if they ever shared a net id.
        let consumer_name = format!("signal-inbound-{}-{}", self.workspace_id, self.net_id);

        let consumer_config = ConsumerConfig {
            durable_name: Some(consumer_name.clone()),
            filter_subject: filter.clone(),
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::New,
            ..Default::default()
        };

        let consumer = stream
            .get_or_create_consumer(&consumer_name, consumer_config)
            .await
            .map_err(|e| SignalListenerError::Consumer(e.to_string()))?;

        let handler = SignalListenerHandler {
            epoch: &self.epoch,
            workspace_id: &self.workspace_id,
            net_id: &self.net_id,
            service: &service,
            eval_notify: &eval_notify,
        };

        run_message_loop_cancellable(
            consumer,
            &handler,
            cancel,
            Some(crate::dlq::DlqPublisher::new(self.jetstream.clone())),
        )
        .await
        .map_err(|e| SignalListenerError::Consumer(e.to_string()))
    }
}

/// Ephemeral handler that borrows from [`SignalListener::run`] arguments.
struct SignalListenerHandler<'a, E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    epoch: &'a AtomicU64,
    workspace_id: &'a str,
    net_id: &'a str,
    service: &'a Arc<PetriNetService<E, T, S>>,
    eval_notify: &'a Notify,
}

#[async_trait::async_trait]
impl<E, T, S> MessageHandler for SignalListenerHandler<'_, E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    fn listener_name(&self) -> &str {
        "signal"
    }

    fn pre_process(&self, msg: &Message) -> PreProcessResult {
        let current_epoch = self.epoch.load(Ordering::SeqCst);
        if current_epoch > 0 {
            if let Ok(info) = msg.info() {
                if info.stream_sequence <= current_epoch {
                    tracing::debug!(
                        stream_sequence = info.stream_sequence,
                        epoch = current_epoch,
                        workspace_id = %self.workspace_id,
                        net_id = %self.net_id,
                        "Signal listener: skipping stale message (pre-epoch)"
                    );
                    return PreProcessResult::Skip;
                }
            }
        }
        PreProcessResult::Continue
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let subject = msg.subject.as_str();

        // Parse subject to extract target place name. The filter already pins
        // ws + net_id to this listener's own net, so we only need the place.
        let place_name = Subjects::parse_signal_subject(subject)
            .map(|(_ws, _net_id, place_name)| place_name)
            .ok_or_else(|| {
                ProcessError::Parse(format!("Could not parse signal subject: {}", subject))
            })?;

        // Deserialize the signal
        let signal: ExternalSignal =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        // Resolve place name to PlaceId (string IDs are the domain IDs now)
        let place_id = match self.service.resolve_place_id(place_name) {
            Ok(id) => id,
            Err(_) => {
                return Err(ProcessError::Business(format!(
                    "Target place '{}' not found",
                    place_name
                )));
            }
        };

        // Convert signal payload to TokenColor and inject with signal_key
        // for causality tracking. The signal_key links back to the originating
        // EffectCompleted event via the causality cross-links table.
        let color = json_to_token_color(&signal.payload);
        let sig_key = if signal.signal_key.is_empty() {
            None
        } else {
            Some(signal.signal_key.clone())
        };

        let dedup_id = signal.dedup_id.clone().filter(|s| !s.is_empty());
        match self
            .service
            .create_token_with_meta(place_id, color, None, sig_key, dedup_id)
            .await
        {
            Ok(_) => {}
            Err(ServiceError::EventStore(EventStoreError::Timeout)) => {
                // Event IS already on JetStream — only the local consumer
                // hasn't caught up. Wake the eval loop, then NACK for
                // redelivery (idempotent publish prevents duplicates).
                self.eval_notify.notify_one();
                return Err(ProcessError::Transient(format!(
                    "Consumer apply timeout (signal_key={}); \
                     notified eval + nacking for redelivery",
                    signal.signal_key
                )));
            }
            Err(e) => return Err(ProcessError::Business(e.to_string())),
        }

        tracing::info!(
            workspace_id = %self.workspace_id,
            net_id = %self.net_id,
            source = %signal.source,
            signal_key = %signal.signal_key,
            target_place = %place_name,
            "Signal listener: token injected from external signal"
        );

        // Wake the evaluation loop
        self.eval_notify.notify_one();
        Ok(())
    }
}
