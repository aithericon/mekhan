//! HumanResultListener: receives human task results via NATS and injects tokens.
//!
//! Creates three durable consumers for completed, cancelled, and failed results.
//! Each consumer injects a token into the target place with a `status` field
//! differentiating the result type.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_nats::jetstream::consumer::{pull::Config as ConsumerConfig, AckPolicy, DeliverPolicy};
use async_nats::jetstream::stream::{Config as StreamConfig, RetentionPolicy};
use async_nats::jetstream::Message;
use petri_application::{
    json_to_token_color, EventRepository, EventStoreError, PetriNetService, ServiceError,
    StateProjection, TopologyRepository,
};
use petri_domain::human::{HumanTaskCancellation, HumanTaskCompletion, HumanTaskFailure};
use petri_domain::PlaceId;
use std::time::Duration;

use tokio::sync::Notify;

use crate::message_loop::{
    run_message_loop_cancellable, MessageHandler, PreProcessResult, ProcessError,
};
use crate::subjects::Subjects;

/// Errors from the human result listener.
#[derive(Debug, thiserror::Error)]
pub enum HumanResultListenerError {
    #[error("JetStream error: {0}")]
    JetStream(String),

    #[error("Consumer error: {0}")]
    Consumer(String),
}

/// Listener that receives human task results via NATS and injects tokens.
///
/// One instance per net. Creates three durable consumers:
/// - completed: `human.completed.{net_id}.>`
/// - cancelled: `human.cancelled.{net_id}.>`
/// - failed: `human.failed.{net_id}.>`
///
/// Each result is injected as a token with a `status` field.
pub struct HumanResultListener {
    net_id: String,
    /// Workspace (tenant) of this net. Human result/cancel/fail subjects are
    /// ws-segmented (`human.{ws}.{category}.{net}.{place}`), so the per-net
    /// consumer filters on `human.{ws}.{category}.{net}.>`. Defaults to the
    /// reserved DEFAULT_WORKSPACE sentinel until stamped via [`Self::with_workspace`].
    workspace_id: String,
    jetstream: async_nats::jetstream::Context,
    epoch: Arc<AtomicU64>,
}

impl HumanResultListener {
    pub fn new(net_id: String, jetstream: async_nats::jetstream::Context) -> Self {
        Self {
            net_id,
            workspace_id: Subjects::DEFAULT_WORKSPACE.to_string(),
            jetstream,
            epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Stamp this net's workspace so the per-net consumers filter on the
    /// correct `human.{ws}.{category}.{net}.>` subjects.
    pub fn with_workspace(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = workspace_id.into();
        self
    }

    /// Advance the epoch for scenario reload filtering.
    pub async fn advance_epoch(&self) -> Result<(), HumanResultListenerError> {
        // Advance epoch on the completed stream (the primary inbound stream)
        let stream = self
            .jetstream
            .get_or_create_stream(completed_stream_config())
            .await
            .map_err(|e| HumanResultListenerError::JetStream(e.to_string()))?;

        let mut stream = stream;
        let info = stream
            .info()
            .await
            .map_err(|e| HumanResultListenerError::JetStream(e.to_string()))?;
        let new_epoch = info.state.last_sequence;
        let old = self.epoch.swap(new_epoch, Ordering::SeqCst);
        tracing::info!(
            net_id = %self.net_id,
            old_epoch = old,
            new_epoch = new_epoch,
            "Advanced human result listener epoch for scenario reload"
        );
        Ok(())
    }

    /// Start all three consumer loops as spawned tokio tasks.
    pub fn start<E, T, S>(
        self: Arc<Self>,
        service: Arc<PetriNetService<E, T, S>>,
        eval_notify: Arc<Notify>,
    ) -> Vec<tokio::task::JoinHandle<()>>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let mut handles = Vec::new();

        // Completed consumer
        {
            let listener = self.clone();
            let svc = service.clone();
            let notify = eval_notify.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = listener.run_completed_consumer(svc, notify).await {
                    tracing::error!(
                        error = %e,
                        net_id = %listener.net_id,
                        "Human completed consumer stopped with error"
                    );
                }
            }));
        }

        // Cancelled consumer
        {
            let listener = self.clone();
            let svc = service.clone();
            let notify = eval_notify.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = listener.run_cancelled_consumer(svc, notify).await {
                    tracing::error!(
                        error = %e,
                        net_id = %listener.net_id,
                        "Human cancelled consumer stopped with error"
                    );
                }
            }));
        }

        // Failed consumer
        {
            let listener = self.clone();
            let svc = service.clone();
            let notify = eval_notify.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = listener.run_failed_consumer(svc, notify).await {
                    tracing::error!(
                        error = %e,
                        net_id = %listener.net_id,
                        "Human failed consumer stopped with error"
                    );
                }
            }));
        }

        handles
    }

    async fn run_completed_consumer<E, T, S>(
        &self,
        service: Arc<PetriNetService<E, T, S>>,
        eval_notify: Arc<Notify>,
    ) -> Result<(), HumanResultListenerError>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let stream = self
            .jetstream
            .get_or_create_stream(completed_stream_config())
            .await
            .map_err(|e| HumanResultListenerError::JetStream(e.to_string()))?;

        let filter = Subjects::human_completed_filter(&self.workspace_id, &self.net_id);
        let consumer_name = format!("human-completed-{}", self.net_id);

        let consumer_config = ConsumerConfig {
            durable_name: Some(consumer_name.clone()),
            filter_subject: filter,
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::New,
            ..Default::default()
        };

        let consumer = stream
            .get_or_create_consumer(&consumer_name, consumer_config)
            .await
            .map_err(|e| HumanResultListenerError::Consumer(e.to_string()))?;

        let handler = CompletedHandler {
            epoch: &self.epoch,
            net_id: &self.net_id,
            service: &service,
            eval_notify: &eval_notify,
        };

        run_message_loop_cancellable(consumer, &handler, None, None)
            .await
            .map_err(|e| HumanResultListenerError::Consumer(e.to_string()))
    }

    async fn run_cancelled_consumer<E, T, S>(
        &self,
        service: Arc<PetriNetService<E, T, S>>,
        eval_notify: Arc<Notify>,
    ) -> Result<(), HumanResultListenerError>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let stream = self
            .jetstream
            .get_or_create_stream(cancelled_stream_config())
            .await
            .map_err(|e| HumanResultListenerError::JetStream(e.to_string()))?;

        let filter = Subjects::human_cancelled_filter(&self.workspace_id, &self.net_id);
        let consumer_name = format!("human-cancelled-{}", self.net_id);

        let consumer_config = ConsumerConfig {
            durable_name: Some(consumer_name.clone()),
            filter_subject: filter,
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::New,
            ..Default::default()
        };

        let consumer = stream
            .get_or_create_consumer(&consumer_name, consumer_config)
            .await
            .map_err(|e| HumanResultListenerError::Consumer(e.to_string()))?;

        let handler = CancelledHandler {
            epoch: &self.epoch,
            net_id: &self.net_id,
            service: &service,
            eval_notify: &eval_notify,
        };

        run_message_loop_cancellable(consumer, &handler, None, None)
            .await
            .map_err(|e| HumanResultListenerError::Consumer(e.to_string()))
    }

    async fn run_failed_consumer<E, T, S>(
        &self,
        service: Arc<PetriNetService<E, T, S>>,
        eval_notify: Arc<Notify>,
    ) -> Result<(), HumanResultListenerError>
    where
        E: EventRepository + 'static,
        T: TopologyRepository + 'static,
        S: StateProjection + 'static,
    {
        let stream = self
            .jetstream
            .get_or_create_stream(failed_stream_config())
            .await
            .map_err(|e| HumanResultListenerError::JetStream(e.to_string()))?;

        let filter = Subjects::human_failed_filter(&self.workspace_id, &self.net_id);
        let consumer_name = format!("human-failed-{}", self.net_id);

        let consumer_config = ConsumerConfig {
            durable_name: Some(consumer_name.clone()),
            filter_subject: filter,
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::New,
            ..Default::default()
        };

        let consumer = stream
            .get_or_create_consumer(&consumer_name, consumer_config)
            .await
            .map_err(|e| HumanResultListenerError::Consumer(e.to_string()))?;

        let handler = FailedHandler {
            epoch: &self.epoch,
            net_id: &self.net_id,
            service: &service,
            eval_notify: &eval_notify,
        };

        run_message_loop_cancellable(consumer, &handler, None, None)
            .await
            .map_err(|e| HumanResultListenerError::Consumer(e.to_string()))
    }
}

// ==================== Stream Configs ====================

/// Stream name constant for human completed results.
pub const STREAM_HUMAN_COMPLETED: &str = "HUMAN_COMPLETED";

// TODO(phase2): these are single global streams capturing the human result
// subjects of ALL workspaces. Human subjects are ws-segmented
// (`human.{ws}.{category}.{net}.{place}`), so each stream captures a
// `human.*.{category}.>` wildcard over the workspace token. Phase 2 may shard
// per-workspace.
fn completed_stream_config() -> StreamConfig {
    StreamConfig {
        name: STREAM_HUMAN_COMPLETED.to_string(),
        subjects: vec![format!(
            "{}.*.{}.>",
            Subjects::HUMAN_ROOT,
            Subjects::HUMAN_COMPLETED_CATEGORY
        )],
        retention: RetentionPolicy::Limits,
        max_age: Duration::from_secs(7 * 24 * 60 * 60),
        ..Default::default()
    }
}

fn cancelled_stream_config() -> StreamConfig {
    StreamConfig {
        name: Subjects::STREAM_HUMAN_CANCELLED.to_string(),
        subjects: vec![format!(
            "{}.*.{}.>",
            Subjects::HUMAN_ROOT,
            Subjects::HUMAN_CANCELLED_CATEGORY
        )],
        retention: RetentionPolicy::Limits,
        max_age: Duration::from_secs(7 * 24 * 60 * 60),
        ..Default::default()
    }
}

fn failed_stream_config() -> StreamConfig {
    StreamConfig {
        name: Subjects::STREAM_HUMAN_FAILED.to_string(),
        subjects: vec![format!(
            "{}.*.{}.>",
            Subjects::HUMAN_ROOT,
            Subjects::HUMAN_FAILED_CATEGORY
        )],
        retention: RetentionPolicy::Limits,
        max_age: Duration::from_secs(7 * 24 * 60 * 60),
        ..Default::default()
    }
}

// ==================== Epoch check helper ====================

fn check_epoch(epoch: &AtomicU64, msg: &Message, net_id: &str) -> PreProcessResult {
    let current_epoch = epoch.load(Ordering::SeqCst);
    if current_epoch > 0 {
        if let Ok(info) = msg.info() {
            if info.stream_sequence <= current_epoch {
                tracing::debug!(
                    stream_sequence = info.stream_sequence,
                    epoch = current_epoch,
                    net_id = %net_id,
                    "Human result listener: skipping stale message (pre-epoch)"
                );
                return PreProcessResult::Skip;
            }
        }
    }
    PreProcessResult::Continue
}

// ==================== Place resolution helper ====================

fn resolve_place<E, T, S>(
    place_name: &str,
    service: &PetriNetService<E, T, S>,
) -> Result<PlaceId, ProcessError>
where
    E: EventRepository,
    T: TopologyRepository,
    S: StateProjection,
{
    // String IDs are the domain IDs now
    service
        .resolve_place_id(place_name)
        .map_err(|_| ProcessError::Business(format!("Target place '{}' not found", place_name)))
}

/// Map a `create_token_with_meta` result for human-result handlers.
///
/// On `EventStore(Timeout)` the event was published to JetStream but the local
/// consumer didn't apply it within the timeout. Wake the eval loop and return
/// `Transient` so the message is NACKed for redelivery; idempotent publish in
/// `event_store::append` makes the retry safe (no duplicate TokenCreated).
fn handle_inject_result(
    res: Result<petri_domain::PersistedEvent, ServiceError>,
    eval_notify: &Notify,
    task_id: &str,
) -> Result<(), ProcessError> {
    match res {
        Ok(_) => Ok(()),
        Err(ServiceError::EventStore(EventStoreError::Timeout)) => {
            eval_notify.notify_one();
            Err(ProcessError::Transient(format!(
                "Human-result consumer apply timeout (task_id={}); \
                 notified eval + nacking for redelivery",
                task_id
            )))
        }
        Err(e) => Err(ProcessError::Business(e.to_string())),
    }
}

// ==================== Completed Handler ====================

struct CompletedHandler<'a, E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    epoch: &'a AtomicU64,
    net_id: &'a str,
    service: &'a Arc<PetriNetService<E, T, S>>,
    eval_notify: &'a Notify,
}

#[async_trait::async_trait]
impl<E, T, S> MessageHandler for CompletedHandler<'_, E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    fn listener_name(&self) -> &str {
        "human-completed"
    }

    fn pre_process(&self, msg: &Message) -> PreProcessResult {
        check_epoch(self.epoch, msg, self.net_id)
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let subject = msg.subject.as_str();
        let place_name = Subjects::parse_human_completed_subject(subject)
            .map(|(_ws, _net_id, place)| place)
            .ok_or_else(|| {
                ProcessError::Parse(format!("Could not parse completed subject: {}", subject))
            })?;

        let completion: HumanTaskCompletion =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        let place_id = resolve_place(place_name, self.service)?;

        // Build token data with status field
        let mut token_data = serde_json::json!({
            "status": "completed",
            "task_id": completion.task_id,
            "data": completion.data,
            "completed_at": completion.completed_at.to_rfc3339(),
        });
        if let Some(corr_id) = &completion.corr_id {
            token_data["corr_id"] = serde_json::json!(corr_id);
        }

        let color = json_to_token_color(&token_data);
        let dedup_id = Some(format!("human:complete:{}", completion.task_id));
        let inject = self
            .service
            .create_token_with_meta(
                place_id,
                color,
                None,
                Some(completion.task_id.clone()),
                dedup_id,
            )
            .await;
        handle_inject_result(inject, self.eval_notify, &completion.task_id)?;

        tracing::info!(
            task_id = %completion.task_id,
            target_place = %place_name,
            "Human result listener: completed token injected"
        );

        self.eval_notify.notify_one();
        Ok(())
    }
}

// ==================== Cancelled Handler ====================

struct CancelledHandler<'a, E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    epoch: &'a AtomicU64,
    net_id: &'a str,
    service: &'a Arc<PetriNetService<E, T, S>>,
    eval_notify: &'a Notify,
}

#[async_trait::async_trait]
impl<E, T, S> MessageHandler for CancelledHandler<'_, E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    fn listener_name(&self) -> &str {
        "human-cancelled"
    }

    fn pre_process(&self, msg: &Message) -> PreProcessResult {
        check_epoch(self.epoch, msg, self.net_id)
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let subject = msg.subject.as_str();
        let place_name = Subjects::parse_human_cancelled_subject(subject)
            .map(|(_ws, _net_id, place)| place)
            .ok_or_else(|| {
                ProcessError::Parse(format!("Could not parse cancelled subject: {}", subject))
            })?;

        let cancellation: HumanTaskCancellation =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        let place_id = resolve_place(place_name, self.service)?;

        let mut token_data = serde_json::json!({
            "status": "cancelled",
            "task_id": cancellation.task_id,
            "cancelled_at": cancellation.cancelled_at.to_rfc3339(),
        });
        if let Some(reason) = &cancellation.reason {
            token_data["reason"] = serde_json::json!(reason);
        }

        let color = json_to_token_color(&token_data);
        let dedup_id = Some(format!("human:cancel:{}", cancellation.task_id));
        let inject = self
            .service
            .create_token_with_meta(
                place_id,
                color,
                None,
                Some(cancellation.task_id.clone()),
                dedup_id,
            )
            .await;
        handle_inject_result(inject, self.eval_notify, &cancellation.task_id)?;

        tracing::info!(
            task_id = %cancellation.task_id,
            target_place = %place_name,
            "Human result listener: cancelled token injected"
        );

        self.eval_notify.notify_one();
        Ok(())
    }
}

// ==================== Failed Handler ====================

struct FailedHandler<'a, E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    epoch: &'a AtomicU64,
    net_id: &'a str,
    service: &'a Arc<PetriNetService<E, T, S>>,
    eval_notify: &'a Notify,
}

#[async_trait::async_trait]
impl<E, T, S> MessageHandler for FailedHandler<'_, E, T, S>
where
    E: EventRepository + 'static,
    T: TopologyRepository + 'static,
    S: StateProjection + 'static,
{
    fn listener_name(&self) -> &str {
        "human-failed"
    }

    fn pre_process(&self, msg: &Message) -> PreProcessResult {
        check_epoch(self.epoch, msg, self.net_id)
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let subject = msg.subject.as_str();
        let place_name = Subjects::parse_human_failed_subject(subject)
            .map(|(_ws, _net_id, place)| place)
            .ok_or_else(|| {
                ProcessError::Parse(format!("Could not parse failed subject: {}", subject))
            })?;

        let failure: HumanTaskFailure =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        let place_id = resolve_place(place_name, self.service)?;

        let mut token_data = serde_json::json!({
            "status": "failed",
            "task_id": failure.task_id,
            "failed_at": failure.failed_at.to_rfc3339(),
        });
        if let Some(corr_id) = &failure.corr_id {
            token_data["corr_id"] = serde_json::json!(corr_id);
        }
        if let Some(reason) = &failure.reason {
            token_data["reason"] = serde_json::json!(reason);
        }

        let color = json_to_token_color(&token_data);
        let dedup_id = Some(format!("human:fail:{}", failure.task_id));
        let inject = self
            .service
            .create_token_with_meta(
                place_id,
                color,
                None,
                Some(failure.task_id.clone()),
                dedup_id,
            )
            .await;
        handle_inject_result(inject, self.eval_notify, &failure.task_id)?;

        tracing::info!(
            task_id = %failure.task_id,
            target_place = %place_name,
            "Human result listener: failed token injected"
        );

        self.eval_notify.notify_one();
        Ok(())
    }
}
