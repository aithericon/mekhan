//! NATS-backed [`BatchSink`] — the durable transport for sink-mode crawl
//! batches (docs/32 batch-fold).
//!
//! Publishes each [`FoldBatch`] to the `INVENTORY_FOLD` JetStream stream
//! (subject `inventory.fold.batch`) and waits for the publish ack, so the
//! crawl op's resume cursor only ever advances past durably-accepted batches.
//! Unlike [`publish_event`](crate::event_emitter::publish_event) (status /
//! events, fire-and-forget with logged errors), a failed publish here is
//! PROPAGATED — the calling operation must fail rather than silently drop a
//! batch.
//!
//! The sink stamps the runner's serve identity (`runner_id`, falling back to
//! the routing partition — the same precedence the by-reference artifact path
//! and fileserve binding use) onto every batch as `serve_group`, so the fold
//! consumer persists it into inventory provenance and file-server `adopt`
//! can auto-stamp a servable endpoint.

use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::stream;
use async_trait::async_trait;
use tracing::debug;

use aithericon_executor_backend::traits::BatchSink;
use aithericon_executor_domain::{FoldBatch, INVENTORY_FOLD_STREAM, INVENTORY_FOLD_SUBJECT};

use crate::event_emitter::{stream_name_for, subject_for};

/// Durable NATS JetStream batch sink.
#[derive(Clone)]
pub struct NatsBatchSink {
    jetstream: jetstream::Context,
    subject_prefix: Option<String>,
    /// Runner serve identity stamped onto every published batch.
    serve_group: Option<String>,
}

impl NatsBatchSink {
    /// Create the sink and ensure the `INVENTORY_FOLD` stream exists.
    ///
    /// Race-safe get-then-create (NOT `get_or_create_stream`): mekhan's
    /// `inventory_fold_consumer` ensures the same stream, and
    /// `get_or_create`'s byte-identical-config requirement would couple the
    /// two binaries' config literals (replica counts differ per deploy).
    /// Whichever side boots first creates; the other just gets.
    pub async fn new(
        jetstream: jetstream::Context,
        replicas: usize,
        subject_prefix: Option<String>,
        serve_group: Option<String>,
    ) -> Result<Self, async_nats::Error> {
        let stream_name = stream_name_for(&subject_prefix, "INVFOLD", INVENTORY_FOLD_STREAM);
        let subjects = vec![subject_for(
            &subject_prefix,
            format!("{INVENTORY_FOLD_SUBJECT}.>"),
        )];

        if jetstream.get_stream(&stream_name).await.is_err() {
            let cfg = stream::Config {
                name: stream_name.clone(),
                subjects,
                retention: stream::RetentionPolicy::Limits,
                max_age: Duration::from_secs(7 * 24 * 60 * 60), // 7 days
                duplicate_window: Duration::from_secs(120),
                num_replicas: replicas,
                storage: stream::StorageType::File,
                ..Default::default()
            };
            if let Err(e) = jetstream.create_stream(cfg).await {
                // Lost the create race (mekhan booted concurrently) — fine
                // as long as the stream exists now.
                jetstream.get_stream(&stream_name).await.map_err(|_| e)?;
            }
        }

        debug!(%stream_name, "inventory fold stream ready");

        Ok(Self {
            jetstream,
            subject_prefix,
            serve_group,
        })
    }
}

#[async_trait]
impl BatchSink for NatsBatchSink {
    async fn publish(&self, batch: &FoldBatch) -> Result<(), String> {
        // Stamp the runner identity without forcing the caller to know it.
        let mut batch = batch.clone();
        if batch.serve_group.is_none() {
            batch.serve_group = self.serve_group.clone();
        }

        let payload =
            serde_json::to_vec(&batch).map_err(|e| format!("serialize fold batch: {e}"))?;

        let mut headers = async_nats::HeaderMap::new();
        headers.insert("Nats-Msg-Id", batch.msg_id().as_str());

        // Per-server subject leaf: lets a future consumer filter one server's
        // campaign without a new stream.
        let subject = subject_for(
            &self.subject_prefix,
            format!(
                "{INVENTORY_FOLD_SUBJECT}.{}",
                aithericon_executor_domain::sanitize_subject_token(&batch.file_server_id)
            ),
        );

        let ack = self
            .jetstream
            .publish_with_headers(subject.clone(), headers, payload.into())
            .await
            .map_err(|e| format!("publish fold batch to {subject}: {e}"))?;
        ack.await
            .map_err(|e| format!("fold batch ack on {subject}: {e}"))?;

        debug!(
            execution_id = %batch.execution_id,
            batch_idx = batch.batch_idx,
            items = batch.items.len(),
            %subject,
            "fold batch published"
        );
        Ok(())
    }
}
