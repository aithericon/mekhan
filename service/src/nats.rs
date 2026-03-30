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
                    deliver_policy: jetstream::consumer::DeliverPolicy::New,
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
}
