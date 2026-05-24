use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::consumer::PullConsumer;
use async_nats::jetstream::stream::PurgeResponse;

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

    /// Create or get the durable consumer for Mekhan lifecycle events.
    /// Filters on `petri.events.*.net.>` to catch NetCompleted/NetCancelled.
    /// Note: NATS `*` matches an entire dot-delimited token; net IDs like
    /// `mekhan-{uuid}` are single tokens (no dots), so `*` matches them.
    pub async fn lifecycle_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        let stream = self.jetstream.get_stream("PETRI_GLOBAL").await?;
        let durable = self.durable_name("mekhan-lifecycle");
        let consumer = stream
            .get_or_create_consumer(
                &durable,
                jetstream::consumer::pull::Config {
                    durable_name: Some(durable.clone()),
                    filter_subject: "petri.events.*.net.>".into(),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    deliver_policy: self.deliver_policy(),
                    ..Default::default()
                },
            )
            .await?;
        Ok(consumer)
    }

    /// Purge all event data for a specific net from PETRI_GLOBAL stream.
    pub async fn purge_net_events(&self, net_id: &str) -> Result<PurgeResponse, async_nats::Error> {
        let stream = self.jetstream.get_stream("PETRI_GLOBAL").await?;
        let resp = stream
            .purge()
            .filter(&format!("petri.events.{net_id}.>"))
            .await?;
        Ok(resp)
    }

    /// Purge all signal data for a specific net from PETRI_GLOBAL stream.
    pub async fn purge_net_signals(
        &self,
        net_id: &str,
    ) -> Result<PurgeResponse, async_nats::Error> {
        let stream = self.jetstream.get_stream("PETRI_GLOBAL").await?;
        let resp = stream
            .purge()
            .filter(&format!("petri.signal.{net_id}.>"))
            .await?;
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
                name: "HUMAN_REQUESTS".into(),
                subjects: vec!["human.request.>".into()],
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
                name: "MEKHAN_SILENT_DROPS".into(),
                subjects: vec!["mekhan.silent_drops.>".into()],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_age: std::time::Duration::from_secs(7 * 24 * 60 * 60), // 7 days
                max_messages: 10_000, // forensic cap; not durable storage
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    /// Create or get the durable consumer for human task request ingestion.
    pub async fn human_task_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        let stream = self.jetstream.get_stream("HUMAN_REQUESTS").await?;
        let durable = self.durable_name("mekhan-human-task-ingest");
        let consumer = stream
            .get_or_create_consumer(
                &durable,
                jetstream::consumer::pull::Config {
                    durable_name: Some(durable.clone()),
                    filter_subject: "human.request.>".into(),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    deliver_policy: self.deliver_policy(),
                    ..Default::default()
                },
            )
            .await?;
        Ok(consumer)
    }

    /// Create or get the durable consumer for causality event ingestion.
    /// Consumes petri domain events and bridge transfers from PETRI_GLOBAL
    /// for causality projection and cross-net link tracking.
    pub async fn causality_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        let stream = self.jetstream.get_stream("PETRI_GLOBAL").await?;
        let durable = self.durable_name("mekhan-causality-ingest");
        let consumer = stream
            .get_or_create_consumer(
                &durable,
                jetstream::consumer::pull::Config {
                    durable_name: Some(durable.clone()),
                    filter_subjects: vec![
                        "petri.events.>".into(),
                        "petri.bridge.>".into(),
                    ],
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    deliver_policy: self.deliver_policy(),
                    ..Default::default()
                },
            )
            .await?;
        Ok(consumer)
    }

    /// Create or get the durable consumer for the step-executions projection.
    /// Consumes `petri.events.>` and folds events into per-step rows via the
    /// projector in `service/src/projections/step_executions/`.
    pub async fn step_executions_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        let stream = self.jetstream.get_stream("PETRI_GLOBAL").await?;
        let durable = self.durable_name("mekhan-step-executions");
        let consumer = stream
            .get_or_create_consumer(
                &durable,
                jetstream::consumer::pull::Config {
                    durable_name: Some(durable.clone()),
                    filter_subject: "petri.events.>".into(),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    deliver_policy: self.deliver_policy(),
                    ..Default::default()
                },
            )
            .await?;
        Ok(consumer)
    }

}
