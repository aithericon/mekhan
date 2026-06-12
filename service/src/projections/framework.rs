//! Shared driver for NATS-fed event-log projections.
//!
//! Every projection under `service/src/projections/` consumes engine events
//! from `PETRI_GLOBAL` (`petri.events.{net_id}.{type}.{subtype}`) and folds
//! them into Postgres rows. The per-projection consumers used to be clones of
//! one loop (durable pull consumer → per-net buffer → whole-log refold →
//! upsert → ack); this module is that loop, written once:
//!
//! - [`Projection`] — what differs per projection: the consumer spec, the
//!   net-id gate, and the fold (`bootstrap` + incremental `apply`, or a
//!   per-event [`Projection::apply_stateless`] for projections whose every
//!   matching event is a self-contained row update).
//! - [`run_projection`] — the driver: batched pull loop, per-net grouping
//!   (concurrent across nets, strictly sequential within one), LRU-bounded
//!   per-net state cache with replay-on-miss bootstrap, dup skip,
//!   terminal-event eviction, ack-on-success / NAK-with-delay on failure.
//!   Delivered sequence gaps are applied incrementally — subject filters make
//!   them the steady state (see [`StepAction::Apply`]).
//! - [`HistoryLoader`] — seam over [`crate::petri::events::fetch_events`] so
//!   the per-event decision core ([`step_event`]) is unit-testable without
//!   NATS.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use sqlx::PgPool;

use petri_domain::{DomainEvent, PersistedEvent};

use crate::nats::consumer::ConsumerSpec;
use crate::nats::MekhanNats;
use crate::observability::record_silent_drop_with;
use crate::petri::events::fetch_events;

/// How a projection (re)builds per-net state on a cache miss.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BootstrapPolicy {
    /// Fetch the net's full event history and fold it via
    /// [`Projection::bootstrap`]; subsequent events fold incrementally via
    /// [`Projection::apply`].
    ReplayHistory,
    /// Every matching event is a self-contained row update: no per-net state,
    /// no history fetch, no terminal tracking. Only
    /// [`Projection::apply_stateless`] runs.
    Stateless,
}

/// Driver knobs. Defaults mirror the step-executions consumer's
/// incident-tuned batch cap (16 messages stays comfortably inside a 120s
/// `ack_wait`) and the historical `MAX_BUFFERED_NETS`.
pub struct DriverTuning {
    /// Max messages pulled (and processed) per batch.
    pub batch: usize,
    /// Upper bound on simultaneously-cached nets; overflow evicts the
    /// least-recently-used entry (it re-bootstraps on its next event).
    pub max_buffered_nets: usize,
    /// Max net-groups of one batch processed concurrently.
    pub max_concurrent_nets: usize,
}

impl Default for DriverTuning {
    fn default() -> Self {
        Self {
            batch: 16,
            max_buffered_nets: 256,
            max_concurrent_nets: 8,
        }
    }
}

/// One event-log projection: declares its durable consumer and folds events
/// into Postgres. Driven by [`run_projection`].
#[async_trait]
pub trait Projection: Send + Sync + 'static {
    /// Per-net fold state carried between events (token-id dedup sets,
    /// correlation maps, …). Use `()` for [`BootstrapPolicy::Stateless`].
    type State: Send + 'static;

    /// Short name used in logs and silent-drop kinds (`{name}_envelope`).
    fn name(&self) -> &'static str;

    /// The durable pull-consumer spec (stream, durable name, subject filters,
    /// ack tuning, optional `migrate_from` cursor transplant).
    fn spec(&self, nats: &MekhanNats) -> ConsumerSpec;

    fn tuning(&self) -> DriverTuning {
        DriverTuning::default()
    }

    /// Cheap net-id pre-filter (subject token 2). Events for unwanted nets
    /// are ACKed without deserialization.
    fn wants_net(&self, _net_id: &str) -> bool {
        true
    }

    fn bootstrap_policy(&self) -> BootstrapPolicy {
        BootstrapPolicy::ReplayHistory
    }

    /// Build the per-net state by folding the full event history (cache
    /// miss). `Ok(None)` means "this net is not mine" — the event
    /// is ACKed and the net is deliberately NOT cached, so non-matching nets
    /// stay cheap misses instead of holding state.
    ///
    /// `history` is LAZY: run any cheap ownership checks (e.g. an indexed
    /// instance lookup) BEFORE awaiting `history.get()`. Foreign nets —
    /// including high-traffic pool nets — deliver here on every event, and
    /// the JetStream replay behind `get()` is the expensive part the
    /// pre-framework consumers deliberately avoided for them.
    async fn bootstrap(
        &self,
        db: &PgPool,
        net_id: &str,
        history: &LazyHistory<'_>,
    ) -> anyhow::Result<Option<Self::State>>;

    /// Fold one freshly-delivered event into the cached state (incremental
    /// path; the driver guarantees `ev.sequence == last_applied + 1`).
    async fn apply(
        &self,
        db: &PgPool,
        net_id: &str,
        state: &mut Self::State,
        ev: &PersistedEvent,
    ) -> anyhow::Result<()>;

    /// Self-contained per-event update ([`BootstrapPolicy::Stateless`] only).
    async fn apply_stateless(
        &self,
        _db: &PgPool,
        _net_id: &str,
        _ev: &PersistedEvent,
    ) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Seam over the JetStream full-history fetch so [`step_event`] is
/// unit-testable without NATS.
#[async_trait]
pub trait HistoryLoader: Send + Sync {
    async fn load(&self, net_id: &str) -> anyhow::Result<Vec<PersistedEvent>>;
}

/// Production loader: ephemeral replay consumer on `petri.events.{net_id}.>`.
pub struct NatsHistoryLoader {
    nats: MekhanNats,
}

#[async_trait]
impl HistoryLoader for NatsHistoryLoader {
    async fn load(&self, net_id: &str) -> anyhow::Result<Vec<PersistedEvent>> {
        fetch_events(&self.nats, net_id).await
    }
}

/// One net's full history, fetched at most once and only on first
/// [`LazyHistory::get`]. Lets [`Projection::bootstrap`] reject foreign nets
/// from a cheap DB lookup without paying the JetStream replay.
pub struct LazyHistory<'a> {
    loader: &'a dyn HistoryLoader,
    net_id: &'a str,
    cell: tokio::sync::OnceCell<Vec<PersistedEvent>>,
}

impl<'a> LazyHistory<'a> {
    pub fn new(loader: &'a dyn HistoryLoader, net_id: &'a str) -> Self {
        Self {
            loader,
            net_id,
            cell: tokio::sync::OnceCell::new(),
        }
    }

    pub async fn get(&self) -> anyhow::Result<&[PersistedEvent]> {
        self.cell
            .get_or_try_init(|| self.loader.load(self.net_id))
            .await
            .map(Vec::as_slice)
    }
}

/// Cached per-net fold state + the sequence of the last folded event.
pub struct NetEntry<S> {
    pub last_applied: u64,
    pub state: S,
}

/// Net-id-keyed LRU cache. `get_mut` touches; `insert` evicts the
/// least-recently-USED entry on overflow (O(n) scan — caches are small).
pub struct LruNetCache<V> {
    cap: usize,
    tick: u64,
    map: HashMap<String, (u64, V)>,
}

impl<V> LruNetCache<V> {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            tick: 0,
            map: HashMap::new(),
        }
    }

    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        let tick = self.next_tick();
        self.map.get_mut(key).map(|(t, v)| {
            *t = tick;
            &mut *v
        })
    }

    pub fn insert(&mut self, key: String, value: V) {
        if !self.map.contains_key(&key) && self.map.len() >= self.cap {
            if let Some(victim) = self
                .map
                .iter()
                .min_by_key(|(_, (t, _))| *t)
                .map(|(k, _)| k.clone())
            {
                self.map.remove(&victim);
            }
        }
        let tick = self.next_tick();
        self.map.insert(key, (tick, value));
    }

    pub fn remove(&mut self, key: &str) -> Option<V> {
        self.map.remove(key).map(|(_, v)| v)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// One batch's messages for a single net, in arrival order.
pub struct NetGroup<M> {
    pub net_id: String,
    pub items: Vec<M>,
}

/// Group one batch's messages by net id, preserving both the first-arrival
/// order of nets and the arrival order within each net. Pure.
pub fn plan_batch<M>(msgs: Vec<(String, M)>) -> Vec<NetGroup<M>> {
    let mut groups: Vec<NetGroup<M>> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for (net_id, item) in msgs {
        match index.get(&net_id) {
            Some(&i) => groups[i].items.push(item),
            None => {
                index.insert(net_id.clone(), groups.len());
                groups.push(NetGroup {
                    net_id,
                    items: vec![item],
                });
            }
        }
    }
    groups
}

/// `NetCompleted` / `NetCancelled` / `NetFailed` — the net's log is final,
/// drop its cached state.
pub fn is_terminal(ev: &DomainEvent) -> bool {
    matches!(
        ev,
        DomainEvent::NetCompleted { .. }
            | DomainEvent::NetCancelled { .. }
            | DomainEvent::NetFailed { .. }
    )
}

/// Per-event decision for the [`BootstrapPolicy::ReplayHistory`] path, given
/// the cached `last_applied` (if any) and the delivered sequence. Pure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepAction {
    /// No cached state — fetch history and bootstrap.
    Bootstrap,
    /// `seq <= last_applied` — already folded (bootstrap overlap / JetStream
    /// redelivery); ACK without work.
    SkipDup,
    /// `seq > last_applied` — fold incrementally. Sequence gaps are NORMAL
    /// here, not a missed-event signal: `PersistedEvent.sequence` numbers the
    /// full per-net log while the durable's subject filters exclude
    /// fold-irrelevant event types (e.g. `token.consumed`), so delivered
    /// sequences routinely skip. The per-projection filter-coverage tests pin
    /// that every fold-relevant variant IS delivered, and the two genuine
    /// missed-event hazards both drop the cache entry and take [`Bootstrap`]
    /// instead: LRU eviction (entry gone → miss) and processing failure
    /// (run_projection drops the entry before NAK).
    Apply,
}

pub fn classify(last_applied: Option<u64>, seq: u64) -> StepAction {
    match last_applied {
        None => StepAction::Bootstrap,
        Some(la) if seq <= la => StepAction::SkipDup,
        Some(_) => StepAction::Apply,
    }
}

/// The per-event core: decide + fold one delivered event for one net.
/// `entry` is the net's cached state, owned by the caller for the duration of
/// the net-group (groups touch disjoint nets, so no lock is needed).
pub(crate) async fn step_event<P: Projection>(
    projection: &P,
    loader: &dyn HistoryLoader,
    db: &PgPool,
    entry: &mut Option<NetEntry<P::State>>,
    net_id: &str,
    ev: &PersistedEvent,
) -> anyhow::Result<()> {
    if projection.bootstrap_policy() == BootstrapPolicy::Stateless {
        return projection.apply_stateless(db, net_id, ev).await;
    }

    match classify(entry.as_ref().map(|e| e.last_applied), ev.sequence) {
        StepAction::SkipDup => {}
        StepAction::Apply => {
            let e = entry.as_mut().expect("classify Apply implies cached entry");
            projection.apply(db, net_id, &mut e.state, ev).await?;
            e.last_applied = ev.sequence;
        }
        StepAction::Bootstrap => {
            *entry = None;
            let history = LazyHistory::new(loader, net_id);
            if let Some(state) = projection.bootstrap(db, net_id, &history).await? {
                // A Some(state) bootstrap has folded the history, so the
                // fetch is already cached — this get() never re-fetches.
                let last_applied = history.get().await?.last().map(|e| e.sequence).unwrap_or(0);
                *entry = Some(NetEntry {
                    last_applied,
                    state,
                });
                // The fresh history normally already contains the delivered
                // event; if the fetch raced ahead of the stream, fold it now
                // so it isn't lost.
                if let Some(e) = entry.as_mut() {
                    if ev.sequence > e.last_applied {
                        projection.apply(db, net_id, &mut e.state, ev).await?;
                        e.last_applied = ev.sequence;
                    }
                }
            }
        }
    }

    if is_terminal(&ev.event) {
        *entry = None;
    }
    Ok(())
}

/// Run one projection to completion: create its durable consumer, then pull
/// batches forever. Within a batch, net-groups are processed concurrently
/// (bounded) and strictly sequentially within each net; each message is ACKed
/// on success, NAK(2s)ed on failure together with its same-net tail; the
/// whole batch is awaited before the next pull.
pub async fn run_projection<P: Projection>(projection: P, nats: MekhanNats, db: PgPool) {
    let consumer = match nats.pull_consumer(projection.spec(&nats)).await {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to create {} consumer: {e}", projection.name());
            return;
        }
    };

    let tuning = projection.tuning();

    let messages = match consumer
        .stream()
        .max_messages_per_batch(tuning.batch)
        .messages()
        .await
    {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("failed to start {} message stream: {e}", projection.name());
            return;
        }
    };

    tracing::info!("{} projection ingest started", projection.name());

    let loader = NatsHistoryLoader { nats: nats.clone() };
    let mut cache: LruNetCache<NetEntry<P::State>> = LruNetCache::new(tuning.max_buffered_nets);
    let envelope_kind = format!("{}_envelope", projection.name());

    let mut batches = messages.ready_chunks(tuning.batch);
    while let Some(batch) = batches.next().await {
        // Gate + decode each message; unwanted/undecodable ones are ACKed
        // here (the existing per-consumer behavior, incl. the silent-drop
        // forensic record on a malformed envelope).
        let mut keyed: Vec<(String, (PersistedEvent, async_nats::jetstream::Message))> =
            Vec::with_capacity(batch.len());
        for msg_result in batch {
            let msg = match msg_result {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("{} ingest message error: {e}", projection.name());
                    continue;
                }
            };
            // Subject: petri.events.{net_id}.{type}.{subtype}
            let Some(net_id) = msg.subject.as_str().split('.').nth(2).map(str::to_string) else {
                let _ = msg.ack().await;
                continue;
            };
            if !projection.wants_net(&net_id) {
                let _ = msg.ack().await;
                continue;
            }
            let ev: PersistedEvent = match serde_json::from_slice(&msg.payload) {
                Ok(p) => p,
                Err(e) => {
                    record_silent_drop_with(
                        &envelope_kind,
                        &e,
                        serde_json::json!({
                            "subject": msg.subject.as_str(),
                            "net_id": net_id,
                        }),
                        Some(&msg.payload),
                    );
                    let _ = msg.ack().await;
                    continue;
                }
            };
            keyed.push((net_id, (ev, msg)));
        }

        // Groups touch disjoint nets, so each takes exclusive ownership of
        // its cache entry for the duration of the batch.
        let work: Vec<_> = plan_batch(keyed)
            .into_iter()
            .map(|group| {
                let entry = cache.remove(&group.net_id);
                (group, entry)
            })
            .collect();

        let results = futures::stream::iter(
            work.into_iter()
                .map(|(group, entry)| process_group(&projection, &loader, &db, group, entry)),
        )
        .buffer_unordered(tuning.max_concurrent_nets.max(1))
        .collect::<Vec<_>>()
        .await;

        for (net_id, entry) in results {
            if let Some(entry) = entry {
                cache.insert(net_id, entry);
            }
        }
    }

    tracing::warn!("{} projection ingest stream ended", projection.name());
}

/// Process one net's slice of a batch, strictly in order. ACK each message on
/// success; on the first failure NAK(2s) it AND every later same-net message
/// (folding past a failed event would corrupt the incremental state), and
/// drop the cached entry so redelivery re-bootstraps from the log.
async fn process_group<P: Projection>(
    projection: &P,
    loader: &dyn HistoryLoader,
    db: &PgPool,
    group: NetGroup<(PersistedEvent, async_nats::jetstream::Message)>,
    mut entry: Option<NetEntry<P::State>>,
) -> (String, Option<NetEntry<P::State>>) {
    let mut failed = false;
    for (ev, msg) in group.items {
        if failed {
            nak(&msg).await;
            continue;
        }
        match step_event(projection, loader, db, &mut entry, &group.net_id, &ev).await {
            Ok(()) => {
                let _ = msg.ack().await;
            }
            Err(e) => {
                tracing::error!(
                    net_id = %group.net_id,
                    "{} processing failed: {e}",
                    projection.name()
                );
                nak(&msg).await;
                entry = None;
                failed = true;
            }
        }
    }
    (group.net_id, entry)
}

async fn nak(msg: &async_nats::jetstream::Message) {
    let _ = msg
        .ack_with(async_nats::jetstream::AckKind::Nak(Some(
            Duration::from_secs(2),
        )))
        .await;
}

/// Test-only NATS subject matcher (`*` = one token, `>` = rest). Used by the
/// per-projection "filter list covers every matched variant" tests.
#[cfg(test)]
pub(crate) fn subject_matches(filter: &str, subject: &str) -> bool {
    let mut f = filter.split('.');
    let mut s = subject.split('.');
    loop {
        match (f.next(), s.next()) {
            (Some(">"), Some(_)) => return true,
            (Some("*"), Some(_)) => {}
            (Some(ft), Some(st)) if ft == st => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use chrono::Utc;

    use super::*;
    use crate::nats::subjects::Subjects;
    use crate::nats::StreamSource;

    fn ev(seq: u64) -> PersistedEvent {
        PersistedEvent {
            sequence: seq,
            timestamp: Utc::now(),
            event: DomainEvent::ErrorOccurred {
                message: format!("e{seq}"),
            },
            hash: String::new(),
            previous_hash: None,
        }
    }

    fn terminal(seq: u64) -> PersistedEvent {
        PersistedEvent {
            sequence: seq,
            timestamp: Utc::now(),
            event: DomainEvent::NetCompleted {
                net_id: "net-a".into(),
                terminal_place_id: "p_end".into(),
                exit_code: None,
            },
            hash: String::new(),
            previous_hash: None,
        }
    }

    /// A PgPool that never connects — the recording projection ignores it.
    fn lazy_db() -> PgPool {
        PgPool::connect_lazy("postgres://unused:unused@localhost:1/unused").expect("lazy pool")
    }

    struct MockLoader {
        history: Vec<PersistedEvent>,
        calls: AtomicUsize,
    }

    impl MockLoader {
        fn new(history: Vec<PersistedEvent>) -> Self {
            Self {
                history,
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl HistoryLoader for MockLoader {
        async fn load(&self, _net_id: &str) -> anyhow::Result<Vec<PersistedEvent>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.history.clone())
        }
    }

    #[derive(Default)]
    struct RecordingProjection {
        stateless: bool,
        bootstrap_none: bool,
        bootstraps: AtomicUsize,
        applied: Mutex<Vec<u64>>,
        applied_stateless: Mutex<Vec<u64>>,
    }

    #[async_trait]
    impl Projection for RecordingProjection {
        type State = ();

        fn name(&self) -> &'static str {
            "recording"
        }

        fn spec(&self, _nats: &MekhanNats) -> ConsumerSpec {
            ConsumerSpec {
                stream: StreamSource::ExistingWithRetry(Subjects::STREAM_GLOBAL),
                durable_base: "test-recording",
                filter_subjects: vec![Subjects::EVENTS_ALL.into()],
                ack_wait: None,
                inactive_threshold: None,
                migrate_from: None,
            }
        }

        fn bootstrap_policy(&self) -> BootstrapPolicy {
            if self.stateless {
                BootstrapPolicy::Stateless
            } else {
                BootstrapPolicy::ReplayHistory
            }
        }

        async fn bootstrap(
            &self,
            _db: &PgPool,
            _net_id: &str,
            history: &LazyHistory<'_>,
        ) -> anyhow::Result<Option<()>> {
            self.bootstraps.fetch_add(1, Ordering::SeqCst);
            // Mirror the real projections: reject foreign nets BEFORE the
            // history fetch (the cost the lazy handle exists to avoid).
            if self.bootstrap_none {
                return Ok(None);
            }
            history.get().await?;
            Ok(Some(()))
        }

        async fn apply(
            &self,
            _db: &PgPool,
            _net_id: &str,
            _state: &mut (),
            ev: &PersistedEvent,
        ) -> anyhow::Result<()> {
            self.applied.lock().unwrap().push(ev.sequence);
            Ok(())
        }

        async fn apply_stateless(
            &self,
            _db: &PgPool,
            _net_id: &str,
            ev: &PersistedEvent,
        ) -> anyhow::Result<()> {
            self.applied_stateless.lock().unwrap().push(ev.sequence);
            Ok(())
        }
    }

    #[test]
    fn lru_evicts_least_recently_used_not_insertion_order() {
        let mut cache = LruNetCache::new(2);
        cache.insert("a".into(), 1);
        cache.insert("b".into(), 2);
        // Touch the older entry — "b" is now least recently used.
        assert_eq!(cache.get_mut("a"), Some(&mut 1));
        cache.insert("c".into(), 3);
        assert!(cache.contains("a"), "touched entry must survive");
        assert!(!cache.contains("b"), "LRU entry must be evicted");
        assert!(cache.contains("c"));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn lru_reinsert_existing_key_does_not_evict() {
        let mut cache = LruNetCache::new(2);
        cache.insert("a".into(), 1);
        cache.insert("b".into(), 2);
        cache.insert("a".into(), 10);
        assert_eq!(cache.len(), 2);
        assert!(cache.contains("b"));
        assert_eq!(cache.get_mut("a"), Some(&mut 10));
    }

    #[test]
    fn plan_batch_preserves_per_net_order() {
        let groups = plan_batch(vec![
            ("n1".to_string(), 1),
            ("n2".to_string(), 2),
            ("n1".to_string(), 3),
            ("n3".to_string(), 4),
            ("n2".to_string(), 5),
        ]);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].net_id, "n1");
        assert_eq!(groups[0].items, vec![1, 3]);
        assert_eq!(groups[1].net_id, "n2");
        assert_eq!(groups[1].items, vec![2, 5]);
        assert_eq!(groups[2].net_id, "n3");
        assert_eq!(groups[2].items, vec![4]);
    }

    #[test]
    fn classify_decisions() {
        assert_eq!(classify(None, 1), StepAction::Bootstrap);
        assert_eq!(classify(Some(5), 4), StepAction::SkipDup);
        assert_eq!(classify(Some(5), 5), StepAction::SkipDup);
        assert_eq!(classify(Some(5), 6), StepAction::Apply);
        // Gap: filtered-out event types make non-contiguous delivered
        // sequences the common case — must apply, NOT refetch history.
        assert_eq!(classify(Some(5), 8), StepAction::Apply);
    }

    #[tokio::test]
    async fn dup_is_skipped_without_work() {
        let p = RecordingProjection::default();
        let loader = MockLoader::new(vec![]);
        let db = lazy_db();
        let mut entry = Some(NetEntry {
            last_applied: 5,
            state: (),
        });

        step_event(&p, &loader, &db, &mut entry, "net-a", &ev(5))
            .await
            .unwrap();

        assert_eq!(loader.calls(), 0);
        assert!(p.applied.lock().unwrap().is_empty());
        assert_eq!(entry.as_ref().unwrap().last_applied, 5);
    }

    #[tokio::test]
    async fn next_sequence_applies_incrementally() {
        let p = RecordingProjection::default();
        let loader = MockLoader::new(vec![]);
        let db = lazy_db();
        let mut entry = Some(NetEntry {
            last_applied: 5,
            state: (),
        });

        step_event(&p, &loader, &db, &mut entry, "net-a", &ev(6))
            .await
            .unwrap();

        assert_eq!(loader.calls(), 0);
        assert_eq!(*p.applied.lock().unwrap(), vec![6]);
        assert_eq!(entry.as_ref().unwrap().last_applied, 6);
    }

    #[tokio::test]
    async fn gap_applies_incrementally_without_refetch() {
        let p = RecordingProjection::default();
        let loader = MockLoader::new((1..=8).map(ev).collect());
        let db = lazy_db();
        let mut entry = Some(NetEntry {
            last_applied: 5,
            state: (),
        });

        // seq jumps 5 → 8: the in-between events were filtered out by the
        // durable's subject filters, so this is the steady-state hot path —
        // it must NOT touch the history loader.
        step_event(&p, &loader, &db, &mut entry, "net-a", &ev(8))
            .await
            .unwrap();

        assert_eq!(loader.calls(), 0);
        assert_eq!(p.bootstraps.load(Ordering::SeqCst), 0);
        assert_eq!(*p.applied.lock().unwrap(), vec![8]);
        assert_eq!(entry.as_ref().unwrap().last_applied, 8);
    }

    #[tokio::test]
    async fn miss_bootstraps_then_applies_event_past_history() {
        let p = RecordingProjection::default();
        let loader = MockLoader::new((1..=8).map(ev).collect());
        let db = lazy_db();
        let mut entry = None;

        // Delivered event is NOT in the fetched history (fetch raced ahead).
        step_event(&p, &loader, &db, &mut entry, "net-a", &ev(9))
            .await
            .unwrap();

        assert_eq!(loader.calls(), 1);
        assert_eq!(p.bootstraps.load(Ordering::SeqCst), 1);
        assert_eq!(*p.applied.lock().unwrap(), vec![9]);
        assert_eq!(entry.as_ref().unwrap().last_applied, 9);
    }

    #[tokio::test]
    async fn bootstrap_none_is_not_cached() {
        let p = RecordingProjection {
            bootstrap_none: true,
            ..Default::default()
        };
        let loader = MockLoader::new(vec![ev(1)]);
        let db = lazy_db();
        let mut entry = None;

        step_event(&p, &loader, &db, &mut entry, "net-a", &ev(1))
            .await
            .unwrap();
        assert!(entry.is_none(), "Ok(None) bootstrap must not cache");
        assert!(p.applied.lock().unwrap().is_empty());

        // Stays a cheap miss: the next event re-asks rather than caching —
        // and a foreign-net rejection must never pay the history fetch
        // (pool nets hit this on every event).
        step_event(&p, &loader, &db, &mut entry, "net-a", &ev(2))
            .await
            .unwrap();
        assert_eq!(p.bootstraps.load(Ordering::SeqCst), 2);
        assert_eq!(loader.calls(), 0);
        assert!(entry.is_none());
    }

    #[tokio::test]
    async fn terminal_drops_entry_and_stray_rebootstraps() {
        let p = RecordingProjection::default();
        let loader = MockLoader::new((1..=7).map(ev).collect());
        let db = lazy_db();
        let mut entry = Some(NetEntry {
            last_applied: 5,
            state: (),
        });

        step_event(&p, &loader, &db, &mut entry, "net-a", &terminal(6))
            .await
            .unwrap();
        assert_eq!(*p.applied.lock().unwrap(), vec![6]);
        assert!(entry.is_none(), "terminal event must drop the cache entry");

        // Post-terminal stray (out-of-order redelivery) re-bootstraps.
        step_event(&p, &loader, &db, &mut entry, "net-a", &ev(7))
            .await
            .unwrap();
        assert_eq!(loader.calls(), 1);
        assert_eq!(p.bootstraps.load(Ordering::SeqCst), 1);
        assert!(entry.is_some());
        assert_eq!(entry.as_ref().unwrap().last_applied, 7);
    }

    #[tokio::test]
    async fn stateless_never_calls_history_loader() {
        let p = RecordingProjection {
            stateless: true,
            ..Default::default()
        };
        let loader = MockLoader::new((1..=7).map(ev).collect());
        let db = lazy_db();
        let mut entry = None;

        for seq in [3u64, 4, 9] {
            step_event(&p, &loader, &db, &mut entry, "net-a", &ev(seq))
                .await
                .unwrap();
        }

        assert_eq!(loader.calls(), 0);
        assert_eq!(p.bootstraps.load(Ordering::SeqCst), 0);
        assert!(p.applied.lock().unwrap().is_empty());
        assert_eq!(*p.applied_stateless.lock().unwrap(), vec![3, 4, 9]);
        assert!(entry.is_none());
    }

    #[test]
    fn subject_matcher() {
        assert!(subject_matches(
            "petri.events.*.effect.completed",
            "petri.events.staging-x.effect.completed"
        ));
        assert!(!subject_matches(
            "petri.events.*.effect.completed",
            "petri.events.staging-x.effect.failed"
        ));
        assert!(subject_matches(
            "petri.events.>",
            "petri.events.n.token.created"
        ));
        assert!(!subject_matches("petri.events.>", "petri.bridge.n.x"));
    }
}
