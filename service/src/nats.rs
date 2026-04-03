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
}

impl MekhanNats {
    pub async fn connect(nats_url: &str) -> Result<Self, NatsError> {
        let client = async_nats::connect(nats_url)
            .await
            .map_err(|e| NatsError(e.to_string()))?;
        let jetstream = jetstream::new(client.clone());
        Ok(Self { client, jetstream })
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
        let consumer = stream
            .get_or_create_consumer(
                "mekhan-lifecycle",
                jetstream::consumer::pull::Config {
                    durable_name: Some("mekhan-lifecycle".into()),
                    filter_subject: "petri.events.*.net.>".into(),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    deliver_policy: jetstream::consumer::DeliverPolicy::All,
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

    /// Ensure the CATALOGUE JetStream stream exists.
    pub async fn ensure_catalogue_stream(&self) -> Result<(), async_nats::Error> {
        self.jetstream
            .get_or_create_stream(jetstream::stream::Config {
                name: "CATALOGUE".into(),
                subjects: vec!["catalogue.commands.>".into()],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_messages: 10_000_000,
                max_age: std::time::Duration::from_secs(30 * 24 * 3600), // 30 days
                duplicate_window: std::time::Duration::from_secs(120),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    /// Ensure the PROCESS JetStream stream exists.
    pub async fn ensure_process_stream(&self) -> Result<(), async_nats::Error> {
        self.jetstream
            .get_or_create_stream(jetstream::stream::Config {
                name: "PROCESS".into(),
                subjects: vec![
                    "process.events.>".into(),
                    "process.metrics.>".into(),
                    "process.logs.>".into(),
                ],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_messages: 10_000_000,
                max_age: std::time::Duration::from_secs(30 * 24 * 3600), // 30 days
                duplicate_window: std::time::Duration::from_secs(120),
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    /// Create or get the durable consumer for process event ingestion.
    pub async fn process_event_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        let stream = self.jetstream.get_stream("PROCESS").await?;
        let consumer = stream
            .get_or_create_consumer(
                "mekhan-process-event-ingest",
                jetstream::consumer::pull::Config {
                    durable_name: Some("mekhan-process-event-ingest".into()),
                    filter_subject: "process.events.>".into(),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    deliver_policy: jetstream::consumer::DeliverPolicy::All,
                    ..Default::default()
                },
            )
            .await?;
        Ok(consumer)
    }

    /// Create or get the durable consumer for process metric ingestion.
    pub async fn process_metric_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        let stream = self.jetstream.get_stream("PROCESS").await?;
        let consumer = stream
            .get_or_create_consumer(
                "mekhan-process-metric-ingest",
                jetstream::consumer::pull::Config {
                    durable_name: Some("mekhan-process-metric-ingest".into()),
                    filter_subject: "process.metrics.>".into(),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    deliver_policy: jetstream::consumer::DeliverPolicy::All,
                    ..Default::default()
                },
            )
            .await?;
        Ok(consumer)
    }

    /// Create or get the durable consumer for process log ingestion.
    pub async fn process_log_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        let stream = self.jetstream.get_stream("PROCESS").await?;
        let consumer = stream
            .get_or_create_consumer(
                "mekhan-process-log-ingest",
                jetstream::consumer::pull::Config {
                    durable_name: Some("mekhan-process-log-ingest".into()),
                    filter_subject: "process.logs.>".into(),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    deliver_policy: jetstream::consumer::DeliverPolicy::All,
                    ..Default::default()
                },
            )
            .await?;
        Ok(consumer)
    }

    /// Create or get the durable consumer for catalogue command ingestion.
    pub async fn catalogue_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        let stream = self.jetstream.get_stream("CATALOGUE").await?;
        let consumer = stream
            .get_or_create_consumer(
                "mekhan-catalogue-ingest",
                jetstream::consumer::pull::Config {
                    durable_name: Some("mekhan-catalogue-ingest".into()),
                    filter_subject: "catalogue.commands.register".into(),
                    ack_policy: jetstream::consumer::AckPolicy::Explicit,
                    deliver_policy: jetstream::consumer::DeliverPolicy::All,
                    ..Default::default()
                },
            )
            .await?;
        Ok(consumer)
    }
}
