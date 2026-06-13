//! Global human result listener: replaces per-net human result listeners.
//!
//! Subscribes to `human.*.completed.>`, `human.*.cancelled.>`, and
//! `human.*.failed.>` (all workspaces, all nets) and routes results to the
//! correct net instance, waking it from hibernation if needed. The leading
//! `*` is the `{ws}` segment per ADR-09 shape `human.{ws}.{category}.{net}.{place}`.
//!
//! This mirrors `GlobalSignalListener` but for human task results, solving the
//! hibernation gap: per-net `HumanResultListener` consumers are cancelled on
//! hibernation, so a global listener is needed to wake nets when results arrive.

use std::sync::Arc;

use async_nats::jetstream::consumer::{pull::Config as ConsumerConfig, AckPolicy, DeliverPolicy};
use async_nats::jetstream::stream::{Config as StreamConfig, RetentionPolicy};
use async_nats::jetstream::Message;
use petri_application::json_to_token_color;
use petri_domain::human::{HumanTaskCancellation, HumanTaskCompletion, HumanTaskFailure};
use std::time::Duration;

use crate::global_signal_listener::{NetResolver, SignalInjectError};
use crate::hibernation::ActivityTracker;
use crate::message_loop::{
    run_message_loop_cancellable, MessageHandler, MessageLoopError, ProcessError,
};
use crate::subjects::Subjects;

/// Global listener that subscribes to all human result subjects and routes to nets.
///
/// Replaces per-net `HumanResultListener`, ensuring results wake hibernated nets.
pub struct GlobalHumanResultListener {
    jetstream: async_nats::jetstream::Context,
    resolver: Arc<dyn NetResolver>,
    activity: Option<Arc<ActivityTracker>>,
    consumer_name_prefix: String,
}

impl GlobalHumanResultListener {
    pub fn new(
        jetstream: async_nats::jetstream::Context,
        resolver: Arc<dyn NetResolver>,
        activity: Option<Arc<ActivityTracker>>,
    ) -> Self {
        Self {
            jetstream,
            resolver,
            activity,
            consumer_name_prefix: "global-human".to_string(),
        }
    }

    /// Create a listener with a custom consumer name prefix (useful for tests).
    pub fn with_consumer_name_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.consumer_name_prefix = prefix.into();
        self
    }

    /// Start all three consumer loops as spawned tokio tasks.
    pub fn start(self: Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let mut handles = Vec::new();

        // Completed consumer
        {
            let listener = self.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = listener.run_completed_consumer().await {
                    tracing::error!(error = %e, "Global human completed consumer stopped with error");
                }
            }));
        }

        // Cancelled consumer
        {
            let listener = self.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = listener.run_cancelled_consumer().await {
                    tracing::error!(error = %e, "Global human cancelled consumer stopped with error");
                }
            }));
        }

        // Failed consumer
        {
            let listener = self.clone();
            handles.push(tokio::spawn(async move {
                if let Err(e) = listener.run_failed_consumer().await {
                    tracing::error!(error = %e, "Global human failed consumer stopped with error");
                }
            }));
        }

        handles
    }

    async fn run_completed_consumer(&self) -> Result<(), MessageLoopError> {
        let stream = self
            .jetstream
            .get_or_create_stream(completed_stream_config())
            .await
            .map_err(|e| MessageLoopError::Consumer(format!("Failed to get stream: {}", e)))?;

        // TODO(phase2): per-workspace consumer split — replace this single
        // cross-workspace consumer with one durable per workspace using
        // `Subjects::human_workspace_filter(ws, HUMAN_COMPLETED_CATEGORY)`.
        let filter = all_workspace_human_filter(Subjects::HUMAN_COMPLETED_CATEGORY);
        let consumer_name = format!("{}-completed", self.consumer_name_prefix);

        let consumer_config = ConsumerConfig {
            durable_name: Some(consumer_name.clone()),
            filter_subject: filter,
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::New,
            ..Default::default()
        };

        let consumer = stream
            .create_consumer(consumer_config)
            .await
            .map_err(|e| MessageLoopError::Consumer(format!("Failed to create consumer: {}", e)))?;

        let handler = GlobalCompletedHandler {
            resolver: &self.resolver,
            activity: &self.activity,
        };

        run_message_loop_cancellable(consumer, &handler, None, None).await
    }

    async fn run_cancelled_consumer(&self) -> Result<(), MessageLoopError> {
        let stream = self
            .jetstream
            .get_or_create_stream(cancelled_stream_config())
            .await
            .map_err(|e| MessageLoopError::Consumer(format!("Failed to get stream: {}", e)))?;

        let filter = all_workspace_human_filter(Subjects::HUMAN_CANCELLED_CATEGORY);
        let consumer_name = format!("{}-cancelled", self.consumer_name_prefix);

        let consumer_config = ConsumerConfig {
            durable_name: Some(consumer_name.clone()),
            filter_subject: filter,
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::New,
            ..Default::default()
        };

        let consumer = stream
            .create_consumer(consumer_config)
            .await
            .map_err(|e| MessageLoopError::Consumer(format!("Failed to create consumer: {}", e)))?;

        let handler = GlobalCancelledHandler {
            resolver: &self.resolver,
            activity: &self.activity,
        };

        run_message_loop_cancellable(consumer, &handler, None, None).await
    }

    async fn run_failed_consumer(&self) -> Result<(), MessageLoopError> {
        let stream = self
            .jetstream
            .get_or_create_stream(failed_stream_config())
            .await
            .map_err(|e| MessageLoopError::Consumer(format!("Failed to get stream: {}", e)))?;

        let filter = all_workspace_human_filter(Subjects::HUMAN_FAILED_CATEGORY);
        let consumer_name = format!("{}-failed", self.consumer_name_prefix);

        let consumer_config = ConsumerConfig {
            durable_name: Some(consumer_name.clone()),
            filter_subject: filter,
            ack_policy: AckPolicy::Explicit,
            deliver_policy: DeliverPolicy::New,
            ..Default::default()
        };

        let consumer = stream
            .create_consumer(consumer_config)
            .await
            .map_err(|e| MessageLoopError::Consumer(format!("Failed to create consumer: {}", e)))?;

        let handler = GlobalFailedHandler {
            resolver: &self.resolver,
            activity: &self.activity,
        };

        run_message_loop_cancellable(consumer, &handler, None, None).await
    }
}

// ==================== Workspace-spanning filter ====================

/// Cross-workspace subscription/stream filter for a human-task category:
/// `human.*.{category}.>` (the leading `*` spans `{ws}` per ADR-09).
///
/// TODO(phase2): per-workspace consumer split — this single cross-workspace
/// filter is replaced by `Subjects::human_workspace_filter(ws, category)`
/// (`human.{ws}.{category}.>`) once each tenant gets its own durable.
fn all_workspace_human_filter(category: &str) -> String {
    format!("{}.*.{}.>", Subjects::HUMAN_ROOT, category)
}

// ==================== Stream Configs ====================
// Reuse the same stream configs as the per-net listener. The stream subjects
// span all workspaces (`human.*.{category}.>`) so a single stream captures
// every tenant's human-task results.

fn completed_stream_config() -> StreamConfig {
    StreamConfig {
        name: crate::human_result_listener::STREAM_HUMAN_COMPLETED.to_string(),
        subjects: vec![all_workspace_human_filter(Subjects::HUMAN_COMPLETED_CATEGORY)],
        retention: RetentionPolicy::Limits,
        max_age: Duration::from_secs(7 * 24 * 60 * 60),
        ..Default::default()
    }
}

fn cancelled_stream_config() -> StreamConfig {
    StreamConfig {
        name: Subjects::STREAM_HUMAN_CANCELLED.to_string(),
        subjects: vec![all_workspace_human_filter(Subjects::HUMAN_CANCELLED_CATEGORY)],
        retention: RetentionPolicy::Limits,
        max_age: Duration::from_secs(7 * 24 * 60 * 60),
        ..Default::default()
    }
}

fn failed_stream_config() -> StreamConfig {
    StreamConfig {
        name: Subjects::STREAM_HUMAN_FAILED.to_string(),
        subjects: vec![all_workspace_human_filter(Subjects::HUMAN_FAILED_CATEGORY)],
        retention: RetentionPolicy::Limits,
        max_age: Duration::from_secs(7 * 24 * 60 * 60),
        ..Default::default()
    }
}

// ==================== Shared activity touch helper ====================

fn touch_activity(activity: &Option<Arc<ActivityTracker>>, net_id: &str) {
    if let Some(ref activity) = activity {
        let activity = activity.clone();
        let net_id = net_id.to_string();
        tokio::spawn(async move {
            if let Err(e) = activity.touch(&net_id).await {
                tracing::warn!(
                    net_id = %net_id,
                    error = %e,
                    "Failed to touch activity after human result"
                );
            }
        });
    }
}

/// Map a SignalInjectError into a ProcessError for the human-result handlers,
/// applying the same notify-on-Timeout + Transient-for-redelivery contract as
/// the bridge/signal listeners. Returns Ok(()) on Ok, otherwise an Err that
/// the handler should propagate. On Timeout the eval loop is woken first.
fn map_inject_err(
    res: Result<(), SignalInjectError>,
    target: &Arc<dyn crate::global_signal_listener::SignalTarget>,
    net_id: &str,
    task_id: &str,
    activity: &Option<Arc<ActivityTracker>>,
) -> Result<(), ProcessError> {
    match res {
        Ok(()) => Ok(()),
        Err(SignalInjectError::Timeout) => {
            target.notify_eval();
            touch_activity(activity, net_id);
            Err(ProcessError::Transient(format!(
                "[net={}] Human-result consumer apply timeout (task_id={}); \
                 notified eval + nacking for redelivery",
                net_id, task_id
            )))
        }
        Err(SignalInjectError::Other(e)) => {
            Err(ProcessError::Business(format!("[net={}] {}", net_id, e)))
        }
    }
}

// ==================== Completed Handler ====================

struct GlobalCompletedHandler<'a> {
    resolver: &'a Arc<dyn NetResolver>,
    activity: &'a Option<Arc<ActivityTracker>>,
}

#[async_trait::async_trait]
impl MessageHandler for GlobalCompletedHandler<'_> {
    fn listener_name(&self) -> &str {
        "global-human-completed"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let subject = msg.subject.as_str();
        let (ws, net_id, place_name) =
            Subjects::parse_human_completed_subject(subject).ok_or_else(|| {
                ProcessError::Parse(format!("Could not parse completed subject: {}", subject))
            })?;

        let completion: HumanTaskCompletion =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        // Resolve the net (may wake from hibernation)
        let target = self
            .resolver
            .resolve_net(net_id)
            .await
            .map_err(ProcessError::Business)?;

        // Build token data with status field (same shape as per-net listener)
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
        let inject = target
            .inject_signal_with_meta(
                place_name,
                color,
                None,
                Some(completion.task_id.clone()),
                dedup_id,
            )
            .await;
        map_inject_err(inject, &target, net_id, &completion.task_id, self.activity)?;

        tracing::info!(
            workspace_id = %ws,
            net_id = %net_id,
            task_id = %completion.task_id,
            target_place = %place_name,
            "Global human result: completed token injected"
        );

        touch_activity(self.activity, net_id);
        target.notify_eval();
        Ok(())
    }
}

// ==================== Cancelled Handler ====================

struct GlobalCancelledHandler<'a> {
    resolver: &'a Arc<dyn NetResolver>,
    activity: &'a Option<Arc<ActivityTracker>>,
}

#[async_trait::async_trait]
impl MessageHandler for GlobalCancelledHandler<'_> {
    fn listener_name(&self) -> &str {
        "global-human-cancelled"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let subject = msg.subject.as_str();
        let (ws, net_id, place_name) =
            Subjects::parse_human_cancelled_subject(subject).ok_or_else(|| {
                ProcessError::Parse(format!("Could not parse cancelled subject: {}", subject))
            })?;

        let cancellation: HumanTaskCancellation =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        let target = self
            .resolver
            .resolve_net(net_id)
            .await
            .map_err(ProcessError::Business)?;

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
        let inject = target
            .inject_signal_with_meta(
                place_name,
                color,
                None,
                Some(cancellation.task_id.clone()),
                dedup_id,
            )
            .await;
        map_inject_err(
            inject,
            &target,
            net_id,
            &cancellation.task_id,
            self.activity,
        )?;

        tracing::info!(
            workspace_id = %ws,
            net_id = %net_id,
            task_id = %cancellation.task_id,
            target_place = %place_name,
            "Global human result: cancelled token injected"
        );

        touch_activity(self.activity, net_id);
        target.notify_eval();
        Ok(())
    }
}

// ==================== Failed Handler ====================

struct GlobalFailedHandler<'a> {
    resolver: &'a Arc<dyn NetResolver>,
    activity: &'a Option<Arc<ActivityTracker>>,
}

#[async_trait::async_trait]
impl MessageHandler for GlobalFailedHandler<'_> {
    fn listener_name(&self) -> &str {
        "global-human-failed"
    }

    async fn process_message(&self, msg: &Message) -> Result<(), ProcessError> {
        let subject = msg.subject.as_str();
        let (ws, net_id, place_name) =
            Subjects::parse_human_failed_subject(subject).ok_or_else(|| {
                ProcessError::Parse(format!("Could not parse failed subject: {}", subject))
            })?;

        let failure: HumanTaskFailure =
            serde_json::from_slice(&msg.payload).map_err(|e| ProcessError::Parse(e.to_string()))?;

        let target = self
            .resolver
            .resolve_net(net_id)
            .await
            .map_err(ProcessError::Business)?;

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
        let inject = target
            .inject_signal_with_meta(
                place_name,
                color,
                None,
                Some(failure.task_id.clone()),
                dedup_id,
            )
            .await;
        map_inject_err(inject, &target, net_id, &failure.task_id, self.activity)?;

        tracing::info!(
            workspace_id = %ws,
            net_id = %net_id,
            task_id = %failure.task_id,
            target_place = %place_name,
            "Global human result: failed token injected"
        );

        touch_activity(self.activity, net_id);
        target.notify_eval();
        Ok(())
    }
}
