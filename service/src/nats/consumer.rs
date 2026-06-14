//! Durable pull-consumer builder + the per-projection consumer factories.
//!
//! Every `*_consumer` factory on [`MekhanNats`] shares the same conventions:
//! durable-name prefixing (test isolation), `AckPolicy::Explicit`, and the
//! test-prefix → `DeliverPolicy::New` behavior. [`ConsumerSpec`] +
//! [`MekhanNats::pull_consumer`] centralize them so a factory only declares
//! what differs: the stream source, the durable base name, the subject
//! filters, and the two incident-born knobs (`ack_wait` /
//! `inactive_threshold`). Projections driven by
//! `crate::projections::framework` declare their [`ConsumerSpec`] on the
//! `Projection` impl instead of adding a factory here.

use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::consumer::PullConsumer;

use super::subjects::{
    self, Subjects, BRIDGE_ALL, EVENTS_CATEGORY_ALL, HUMAN_CANCEL_ALL, HUMAN_REQUEST_ALL,
    INFERENCE_METERING_ALL, STREAM_HUMAN_REQUESTS, STREAM_INFERENCE_METERING,
};
use super::MekhanNats;

/// How to resolve the JetStream stream a consumer attaches to.
pub enum StreamSource {
    /// Stream owned by another component (the engine's `PETRI_GLOBAL`):
    /// wait (bounded) for it to appear — see
    /// `MekhanNats::get_stream_with_retry`.
    ExistingWithRetry(&'static str),
    /// Stream must already exist; fail fast otherwise.
    Existing(&'static str),
    /// `get_or_create_stream` with this config (single owner: mekhan).
    EnsureStream(jetstream::stream::Config),
    /// Stream created by WHICHEVER side boots first via a race-safe
    /// get-then-create — not `get_or_create_stream`, whose
    /// byte-identical-config requirement would couple the two binaries'
    /// literals forever.
    RaceCreate(jetstream::stream::Config),
}

/// Declarative spec for one durable pull consumer.
pub struct ConsumerSpec {
    pub stream: StreamSource,
    /// Base durable name; the per-test consumer prefix (if any) is prepended
    /// by `MekhanNats::durable_name`.
    pub durable_base: &'static str,
    /// One entry → `filter_subject`; several → `filter_subjects`.
    pub filter_subjects: Vec<String>,
    /// `None` keeps the JetStream default (30s).
    pub ack_wait: Option<Duration>,
    /// `None` keeps the JetStream default (never reaped).
    pub inactive_threshold: Option<Duration>,
    /// Old durable to transplant the cursor from when a durable is renamed:
    /// if the NEW durable is absent and the old one exists, the new consumer
    /// starts at the old ack floor + 1 and the old durable is best-effort
    /// deleted; if the old one is absent too, the consumer replays from the
    /// beginning (`DeliverPolicy::All`). Test-prefixed durables always start
    /// at `New` regardless. Currently `None` everywhere — projections wire
    /// this when they rename a durable.
    pub migrate_from: Option<&'static str>,
}

impl MekhanNats {
    /// Create (or get) the durable pull consumer described by `spec`.
    pub async fn pull_consumer(
        &self,
        spec: ConsumerSpec,
    ) -> Result<PullConsumer, async_nats::Error> {
        let stream = match spec.stream {
            StreamSource::ExistingWithRetry(name) => self.get_stream_with_retry(name).await?,
            StreamSource::Existing(name) => self.jetstream.get_stream(name).await?,
            StreamSource::EnsureStream(cfg) => self.jetstream.get_or_create_stream(cfg).await?,
            StreamSource::RaceCreate(cfg) => {
                let name = cfg.name.clone();
                match self.jetstream.get_stream(&name).await {
                    Ok(s) => s,
                    Err(_) => match self.jetstream.create_stream(cfg).await {
                        Ok(s) => s,
                        // Lost the create race (the other owner booted
                        // concurrently).
                        Err(_) => self.jetstream.get_stream(&name).await?,
                    },
                }
            }
        };

        let durable = self.durable_name(spec.durable_base);

        // Track whether we transplanted a cursor so the old durable can be
        // reaped after the new one exists.
        let mut migrated_old: Option<&'static str> = None;
        let deliver_policy = match spec.migrate_from {
            Some(old) if self.consumer_prefix().is_none() => {
                if stream.consumer_info(&durable).await.is_ok() {
                    // New durable already exists — `get_or_create_consumer`
                    // below just fetches it; the policy is ignored.
                    jetstream::consumer::DeliverPolicy::All
                } else {
                    match stream.consumer_info(old).await {
                        Ok(info) => {
                            migrated_old = Some(old);
                            jetstream::consumer::DeliverPolicy::ByStartSequence {
                                start_sequence: info.ack_floor.stream_sequence + 1,
                            }
                        }
                        // Old durable absent too — fresh deployment, replay
                        // from the beginning like any other durable.
                        Err(_) => jetstream::consumer::DeliverPolicy::All,
                    }
                }
            }
            _ => self.deliver_policy(),
        };

        let mut config = jetstream::consumer::pull::Config {
            durable_name: Some(durable.clone()),
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            deliver_policy,
            ..Default::default()
        };
        let mut filters = spec.filter_subjects;
        if filters.len() == 1 {
            config.filter_subject = filters.remove(0);
        } else {
            config.filter_subjects = filters;
        }
        if let Some(ack_wait) = spec.ack_wait {
            config.ack_wait = ack_wait;
        }
        if let Some(inactive_threshold) = spec.inactive_threshold {
            config.inactive_threshold = inactive_threshold;
        }

        let consumer = stream.get_or_create_consumer(&durable, config).await?;

        if let Some(old) = migrated_old {
            // Best-effort: the cursor has been transplanted; a leftover old
            // durable would only accumulate pending forever.
            if let Err(e) = stream.delete_consumer(old).await {
                tracing::warn!(old, new = %durable, "failed to delete migrated-from durable: {e}");
            }
        }

        Ok(consumer)
    }

    /// Create or get the durable consumer for Mekhan lifecycle events.
    /// Filters on `petri.events.*.net.>` to catch NetCompleted/NetCancelled.
    /// Note: NATS `*` matches an entire dot-delimited token; net IDs like
    /// `mekhan-{uuid}` are single tokens (no dots), so `*` matches them.
    pub async fn lifecycle_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        self.pull_consumer(ConsumerSpec {
            stream: StreamSource::ExistingWithRetry(Subjects::STREAM_GLOBAL),
            durable_base: "mekhan-lifecycle",
            filter_subjects: vec![subjects::NET_LIFECYCLE_EVENTS_FILTER.into()],
            ack_wait: None,
            inactive_threshold: None,
            migrate_from: None,
        })
        .await
    }

    /// Create or get the durable consumer for human task request ingestion.
    pub async fn human_task_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        self.pull_consumer(ConsumerSpec {
            stream: StreamSource::Existing(STREAM_HUMAN_REQUESTS),
            durable_base: "mekhan-human-task-ingest",
            filter_subjects: vec![HUMAN_REQUEST_ALL.into()],
            ack_wait: None,
            inactive_threshold: None,
            migrate_from: None,
        })
        .await
    }

    /// Create or get the durable consumer for inventory fold batches
    /// (docs/32 batch-fold): sink-mode `crawl` runners publish one
    /// `FoldBatch` per filled batch to `inventory.fold.batch.<server>`;
    /// the fold ingest (`inventory::fold`) upserts each batch set-based into
    /// `file_inventory` (+ catalogue coupling for hash-carrying items).
    ///
    /// The `INVENTORY_FOLD` stream is created by WHICHEVER side boots first
    /// (executor `NatsBatchSink` or this) via a race-safe get-then-create —
    /// not `get_or_create_stream`, whose byte-identical-config requirement
    /// would couple the two binaries' literals forever.
    ///
    /// Consumer conventions mirror the step-executions projection spec:
    /// `ack_wait: 120s` (a 5000-item batch is thousands of statements today —
    /// the per-item loop; a set-based UNNEST rewrite is the flagged 4M-scale
    /// follow-up) and a 30-day `inactive_threshold`.
    pub async fn inventory_fold_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        use aithericon_executor_domain::{INVENTORY_FOLD_STREAM, INVENTORY_FOLD_SUBJECT};

        self.pull_consumer(ConsumerSpec {
            stream: StreamSource::RaceCreate(jetstream::stream::Config {
                name: INVENTORY_FOLD_STREAM.into(),
                subjects: vec![format!("{INVENTORY_FOLD_SUBJECT}.>")],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_age: Duration::from_secs(7 * 24 * 60 * 60),
                duplicate_window: Duration::from_secs(120),
                storage: jetstream::stream::StorageType::File,
                ..Default::default()
            }),
            durable_base: "mekhan-inventory-fold",
            filter_subjects: vec![format!("{INVENTORY_FOLD_SUBJECT}.>")],
            ack_wait: Some(Duration::from_secs(120)),
            inactive_threshold: Some(Duration::from_secs(30 * 24 * 60 * 60)),
            migrate_from: None,
        })
        .await
    }

    /// Create or get the durable consumer for engine-initiated human task
    /// cancellations. The engine publishes to `human.{ws}.cancel.{net_id}.{place}`
    /// when the `human_cancel` effect handler fires (e.g. a Timeout's drain
    /// transition firing when the timer wins). Mekhan reacts by flipping the
    /// hpi_tasks row to `cancelled`, so the task disappears from the inbox
    /// even though the user never clicked Cancel.
    ///
    /// The `HUMAN_CANCEL` stream is owned by the engine
    /// (`engine/.../human_client.rs::ensure_cancel_stream`). We
    /// `get_or_create` defensively so a fresh dev stack where mekhan boots
    /// first doesn't hang.
    pub async fn human_cancel_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        self.pull_consumer(ConsumerSpec {
            stream: StreamSource::EnsureStream(jetstream::stream::Config {
                name: Subjects::STREAM_HUMAN_CANCEL.into(),
                subjects: vec![HUMAN_CANCEL_ALL.into()],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_age: Duration::from_secs(7 * 24 * 60 * 60),
                ..Default::default()
            }),
            durable_base: "mekhan-human-cancel-ingest",
            filter_subjects: vec![HUMAN_CANCEL_ALL.into()],
            ack_wait: None,
            inactive_threshold: None,
            migrate_from: None,
        })
        .await
    }

    /// Create or get the durable consumer for causality event ingestion.
    /// Consumes petri domain events and bridge transfers from PETRI_GLOBAL
    /// for causality projection and cross-net link tracking.
    pub async fn causality_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        self.pull_consumer(ConsumerSpec {
            stream: StreamSource::ExistingWithRetry(Subjects::STREAM_GLOBAL),
            durable_base: "mekhan-causality-ingest",
            // Events + bridge as two DISJOINT filters. Must NOT be
            // `Subjects::EVENTS_ALL` (`petri.>`) here — it subsumes `BRIDGE_ALL`,
            // and JetStream rejects overlapping `filter_subjects` (error 10138),
            // which silently kills the whole causality projection.
            filter_subjects: vec![EVENTS_CATEGORY_ALL.into(), BRIDGE_ALL.into()],
            ack_wait: None,
            // Reap the durable if this projection is ever removed (see the
            // step-executions projection spec for the incident rationale).
            inactive_threshold: Some(Duration::from_secs(30 * 24 * 60 * 60)),
            migrate_from: None,
        })
        .await
    }

    /// Create or get the durable consumer for the inference-metering audit
    /// ledger (model-pool P5, docs/29 §7'). The router publishes one complete
    /// `inference_core::InferenceRequestLog` per request on
    /// `inference.metering.{request_id}` with a plain `client.publish` — this
    /// `get_or_create_stream`'d `INFERENCE_METERING` JetStream stream captures
    /// those subjects, so the projector
    /// (`service/src/projections/inference_metering.rs`) can fold each record
    /// into the durable `inference_request_log` Postgres table.
    ///
    /// `Limits` retention with a 30-day `max_age` — the durable record lives in
    /// Postgres; the stream is the buffered transport + replay-on-restart.
    pub async fn inference_metering_consumer(&self) -> Result<PullConsumer, async_nats::Error> {
        self.pull_consumer(ConsumerSpec {
            stream: StreamSource::EnsureStream(jetstream::stream::Config {
                name: STREAM_INFERENCE_METERING.into(),
                subjects: vec![INFERENCE_METERING_ALL.into()],
                retention: jetstream::stream::RetentionPolicy::Limits,
                max_age: Duration::from_secs(30 * 24 * 60 * 60), // 30 days
                ..Default::default()
            }),
            durable_base: "mekhan-inference-metering",
            filter_subjects: vec![INFERENCE_METERING_ALL.into()],
            ack_wait: None,
            inactive_threshold: None,
            migrate_from: None,
        })
        .await
    }
}
