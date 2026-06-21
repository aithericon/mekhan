//! Mekhan's NATS handle.
//!
//! - [`MekhanNats`] — connection, JetStream context, KV helpers, purge fns
//!   (this file)
//! - [`consumer`] — [`ConsumerSpec`]/[`StreamSource`] builder + the
//!   per-projection `*_consumer` factories
//! - [`subjects`] — subject/stream name constants (engine-canonical ones
//!   re-exported from `petri_api_types::subjects`, service-owned ones
//!   defined there)

pub mod consumer;
pub mod subjects;

pub use consumer::{ConsumerSpec, StreamSource};

use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::stream::PurgeResponse;

use subjects::{
    net_events_filter, net_signals_filter, Subjects, HUMAN_REQUEST_ALL, SILENT_DROPS_ALL,
    STREAM_HUMAN_REQUESTS, STREAM_SILENT_DROPS,
};

#[derive(Debug, thiserror::Error)]
#[error("NATS error: {0}")]
pub struct NatsError(String);

impl From<async_nats::Error> for NatsError {
    fn from(e: async_nats::Error) -> Self {
        NatsError(e.to_string())
    }
}

#[derive(Clone)]
pub struct MekhanNats {
    client: async_nats::Client,
    jetstream: jetstream::Context,
    /// Optional prefix for durable consumer names. Test isolation seam.
    /// Production leaves this `None` so durable names match the existing
    /// baseline. Tests call [`MekhanNats::with_consumer_prefix`] to set a
    /// per-test unique prefix so parallel tests (and a live dev daemon)
    /// don't share a single durable cursor on `PETRI_GLOBAL` /
    /// `HUMAN_REQUESTS`.
    consumer_prefix: Option<String>,
}

impl MekhanNats {
    pub async fn connect(nats_url: &str, nats_creds: Option<&str>) -> Result<Self, NatsError> {
        let options = if let Some(creds_path) = nats_creds {
            let expanded = shellexpand::tilde(creds_path);
            tracing::info!(url = %nats_url, creds = %expanded, "Connecting to NATS with credentials");
            async_nats::ConnectOptions::with_credentials_file(expanded.as_ref())
                .await
                .map_err(|e| NatsError(format!("Failed to load NATS credentials: {e}")))?
        } else {
            async_nats::ConnectOptions::new()
        };

        let client = options
            .ping_interval(Duration::from_secs(20))
            .connection_timeout(Duration::from_secs(10))
            .request_timeout(Some(Duration::from_secs(10)))
            .event_callback(|event| async move {
                use async_nats::Event;
                match event {
                    Event::Disconnected => tracing::warn!("Mekhan NATS disconnected"),
                    Event::Connected => tracing::info!("Mekhan NATS (re)connected"),
                    Event::SlowConsumer(n) => tracing::warn!(n, "Mekhan NATS slow consumer"),
                    other => tracing::debug!(?other, "Mekhan NATS event"),
                }
            })
            .name("mekhan")
            .connect(nats_url)
            .await
            .map_err(|e| NatsError(e.to_string()))?;
        let jetstream = jetstream::new(client.clone());
        Ok(Self {
            client,
            jetstream,
            consumer_prefix: None,
        })
    }

    /// Set a per-test prefix that scopes all durable consumer names this
    /// `MekhanNats` creates. Returns the modified value so it can be used
    /// fluently: `let nats = MekhanNats::connect(...).await?.with_consumer_prefix(prefix);`.
    ///
    /// Each `ensure_*_consumer` call will allocate a durable named
    /// `{prefix}_{base}` (e.g. `test_abc123_mekhan-lifecycle`) instead of
    /// the bare `{base}`. Parallel tests and the dev daemon then keep
    /// independent cursors on shared streams, so the test suite can drop
    /// the `clean_slate` purge ritual.
    pub fn with_consumer_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.consumer_prefix = Some(prefix.into());
        self
    }

    /// Compose a durable consumer name from the base name and the optional
    /// per-test prefix. Production (no prefix) returns `base` unchanged.
    fn durable_name(&self, base: &str) -> String {
        match &self.consumer_prefix {
            Some(prefix) => format!("{prefix}_{base}"),
            None => base.to_string(),
        }
    }

    /// Inspect the active consumer prefix (test-only callers).
    pub fn consumer_prefix(&self) -> Option<&str> {
        self.consumer_prefix.as_deref()
    }

    /// Deliver policy for new durable consumers this `MekhanNats` allocates.
    ///
    /// Production (`consumer_prefix == None`) replays from the beginning of
    /// the stream so a restart catches up on missed events. Tests with a
    /// prefix set need the opposite: a fresh durable that only sees events
    /// emitted after consumer creation, so the test isn't slowed by — or
    /// disturbed by — backlog from other tests / the live dev daemon.
    /// Callers that wire fresh listeners must sequence `spawn → brief sleep
    /// → publish` so messages aren't lost in the (small) race between
    /// `get_or_create_consumer` returning and the pull stream coming up.
    fn deliver_policy(&self) -> jetstream::consumer::DeliverPolicy {
        if self.consumer_prefix.is_some() {
            jetstream::consumer::DeliverPolicy::New
        } else {
            jetstream::consumer::DeliverPolicy::All
        }
    }

    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }

    pub fn jetstream(&self) -> &jetstream::Context {
        &self.jetstream
    }

    /// Publish a cancellation request for one execution onto the
    /// `EXECUTOR_CANCEL` JetStream stream (`executor.cancel.{execution_id}`).
    ///
    /// Cancels ride JetStream, NOT core NATS: core pub/sub interest does not
    /// propagate from mekhan's internal NATS connection to a runner connected
    /// over the Traefik WebSocket front door, so the old `client().publish()`
    /// was silently dropped before reaching the runner (jobs/status/events all
    /// already ride JetStream and cross the boundary fine). The stream is ensured
    /// idempotently here — the runner and engine also `get_or_create` it, so
    /// whichever publishes/binds first wins and the rest are no-ops.
    pub async fn publish_cancel(&self, execution_id: &str) -> Result<(), NatsError> {
        self.jetstream
            .get_or_create_stream(jetstream::stream::Config {
                name: aithericon_executor_domain::cancel_stream_name(None),
                subjects: vec![aithericon_executor_domain::cancel_subject_filter(None)],
                retention: jetstream::stream::RetentionPolicy::Limits,
                storage: jetstream::stream::StorageType::File,
                max_age: Duration::from_secs(
                    aithericon_executor_domain::CANCEL_STREAM_MAX_AGE_SECS,
                ),
                discard: jetstream::stream::DiscardPolicy::Old,
                ..Default::default()
            })
            .await
            .map_err(|e| NatsError(format!("ensure EXECUTOR_CANCEL stream: {e}")))?;

        let subject = aithericon_executor_domain::cancel_subject(execution_id);
        self.jetstream
            .publish(subject, Vec::new().into())
            .await
            .map_err(|e| NatsError(format!("publish cancel: {e}")))?
            .await
            .map_err(|e| NatsError(format!("publish cancel ack: {e}")))?;
        Ok(())
    }

    /// Resolve a JetStream stream, waiting (bounded) for it to be created.
    ///
    /// `PETRI_GLOBAL` is created by the ENGINE (`petri_nats::stream_config`),
    /// never by mekhan. On a cold `just dev` mekhan can boot before the engine
    /// has created it, so a one-shot `get_stream` would `Err` and the consumer
    /// task would log-and-return forever — the projection then silently never
    /// populates until a full mekhan restart. Retry with backoff so the
    /// consumer simply blocks inside its first await until the stream exists.
    ///
    /// The cap (60 attempts, 0.5s→5s backoff → ~2 min worst case) exists so a
    /// genuinely-misconfigured NATS (stream never appears) still surfaces the
    /// original error to the caller's existing `Err => error!; return` arm
    /// rather than hanging the task forever.
    async fn get_stream_with_retry(
        &self,
        name: &str,
    ) -> Result<jetstream::stream::Stream, async_nats::Error> {
        const MAX_ATTEMPTS: u32 = 60;
        let mut delay = Duration::from_millis(500);
        let mut attempt = 0u32;
        loop {
            match self.jetstream.get_stream(name).await {
                Ok(s) => return Ok(s),
                Err(e) => {
                    attempt += 1;
                    if attempt >= MAX_ATTEMPTS {
                        return Err(e.into());
                    }
                    tracing::warn!(
                        stream = name,
                        attempt,
                        "stream not available yet (engine may still be starting); retrying in {delay:?}: {e}"
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(5));
                }
            }
        }
    }

    /// Purge all event data for a specific net from PETRI_GLOBAL stream.
    pub async fn purge_net_events(&self, net_id: &str) -> Result<PurgeResponse, async_nats::Error> {
        let stream = self.jetstream.get_stream(Subjects::STREAM_GLOBAL).await?;
        let resp = stream.purge().filter(net_events_filter(net_id)).await?;
        Ok(resp)
    }

    /// Purge all signal data for a specific net from PETRI_GLOBAL stream.
    pub async fn purge_net_signals(
        &self,
        net_id: &str,
    ) -> Result<PurgeResponse, async_nats::Error> {
        let stream = self.jetstream.get_stream(Subjects::STREAM_GLOBAL).await?;
        let resp = stream.purge().filter(net_signals_filter(net_id)).await?;
        Ok(resp)
    }

    /// Delete metadata KV entry for a net.
    pub async fn delete_net_metadata(&self, net_id: &str) -> Result<(), async_nats::Error> {
        let kv = self.jetstream.get_key_value("KV_NET_METADATA").await?;
        // purge removes the key and all revisions
        kv.purge(net_id).await?;
        Ok(())
    }

    /// Delete activity KV entry for a net.
    pub async fn delete_net_activity(&self, net_id: &str) -> Result<(), async_nats::Error> {
        let kv = self.jetstream.get_key_value("KV_NET_ACTIVITY").await?;
        kv.purge(net_id).await?;
        Ok(())
    }

    /// Ensure the CATALOGUE_SUBSCRIPTIONS KV bucket exists.
    pub async fn ensure_catalogue_subscriptions_kv(
        &self,
    ) -> Result<async_nats::jetstream::kv::Store, async_nats::Error> {
        let kv = self
            .jetstream
            .create_key_value(jetstream::kv::Config {
                bucket: "CATALOGUE_SUBSCRIPTIONS".into(),
                history: 1,
                ..Default::default()
            })
            .await?;
        Ok(kv)
    }

    /// Ensure the TRIGGER_STATE KV bucket exists. Used by the cron source for
    /// last-fire timestamps and by future sources (catalog dedup, etc.) that
    /// need to survive restarts.
    pub async fn ensure_trigger_state_kv(
        &self,
    ) -> Result<async_nats::jetstream::kv::Store, async_nats::Error> {
        let kv = self
            .jetstream
            .create_key_value(jetstream::kv::Config {
                bucket: "TRIGGER_STATE".into(),
                history: 1,
                ..Default::default()
            })
            .await?;
        Ok(kv)
    }

    /// Ensure the HUMAN_REQUESTS JetStream stream exists.
    pub async fn ensure_human_stream(&self) -> Result<(), async_nats::Error> {
        self.jetstream
            .get_or_create_stream(jetstream::stream::Config {
                name: STREAM_HUMAN_REQUESTS.into(),
                subjects: vec![HUMAN_REQUEST_ALL.into()],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_age: std::time::Duration::from_secs(7 * 24 * 60 * 60), // 7 days
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    /// Ensure the MEKHAN_SILENT_DROPS JetStream stream exists.
    ///
    /// Dead-letter queue for messages a consumer couldn't process —
    /// deserialization failures, subject-shape mismatches, KV row
    /// deserialize errors. Bounded retention because silent drops are
    /// (in healthy operation) rare; cap is forensic, not durable
    /// storage. Records are produced by `observability::record_silent_drop*`
    /// via the background drainer.
    pub async fn ensure_silent_drops_stream(&self) -> Result<(), async_nats::Error> {
        self.jetstream
            .get_or_create_stream(jetstream::stream::Config {
                name: STREAM_SILENT_DROPS.into(),
                subjects: vec![SILENT_DROPS_ALL.into()],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_age: std::time::Duration::from_secs(7 * 24 * 60 * 60), // 7 days
                max_messages: 10_000, // forensic cap; not durable storage
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    /// Ensure the shared lab-runner-fleet **dead-letter** stream
    /// (`runner-jobs_dlq`) exists — a CLUSTER-OWNED concern, created here at
    /// mekhan startup with mekhan's broad account creds rather than by an
    /// enrolled runner.
    ///
    /// apalis-nats `NatsStorage` ensures its DLQ stream at init via
    /// `get_or_create_stream`. The per-priority job streams (`runner-jobs_*`)
    /// are created cluster-side by the engine producer when it first dispatches
    /// to a runner, but nothing creates the DLQ — so without this a scoped
    /// (consumer-only, `STREAM.INFO`-only) runner would have to create it
    /// itself, which it must not be allowed to do on a fleet-shared stream.
    /// Pre-creating it here lets the runner JWT stay read-only: its
    /// `get_or_create` resolves via INFO and never attempts a create.
    ///
    /// Config mirrors apalis-nats' DLQ (`apalis-nats/src/storage.rs`): subject
    /// `runner-jobs.dlq`, 30-day Limits retention, file storage — so the
    /// runner's `get_or_create` sees a compatible existing stream.
    pub async fn ensure_runner_jobs_dlq_stream(&self) -> Result<(), async_nats::Error> {
        // Keep in sync with `runners_nats::RUNNER_JOBS_NAMESPACE` ("runner-jobs")
        // and apalis-nats' `{namespace}_dlq` / `{namespace}.dlq` naming.
        self.jetstream
            .get_or_create_stream(jetstream::stream::Config {
                name: "runner-jobs_dlq".into(),
                subjects: vec!["runner-jobs.dlq".into()],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_age: std::time::Duration::from_secs(30 * 24 * 60 * 60), // 30 days
                storage: jetstream::stream::StorageType::File,
                ..Default::default()
            })
            .await?;
        Ok(())
    }
}
