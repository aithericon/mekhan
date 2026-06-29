use std::collections::VecDeque;
use std::sync::RwLock;

use petri_application::net_snapshot::NetSnapshot;
use petri_application::{
    dedup_seed_bytes, DedupSeed, EventRepository, EventStoreError, EventStoreMemory, SnapshotInputs,
};
use petri_domain::{apply_event_to_marking, DomainEvent, Marking, PersistedEvent};

/// Default in-memory tail budget for the per-net event cache. The durable NATS
/// log is the source of truth; the tail only needs enough recent history to
/// serve the eval-loop's incremental marking cursor and the dedup window.
/// Override with `PETRI_MAX_EVENT_TAIL_BYTES`.
pub const DEFAULT_MAX_EVENT_TAIL_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

/// In-memory implementation of the event store, with a bounded memory
/// footprint.
///
/// The store keeps an **incremental base + bounded tail**:
///
/// - **Base** — a *folded* summary of all events before storage position
///   `base_count`: the projected [`Marking`], a dedup seed, the hash chain tip,
///   and the count. The raw events behind the base are dropped from memory
///   (they stay on the durable NATS log).
/// - **Tail** — the most-recent [`PersistedEvent`]s, kept verbatim in a
///   `VecDeque`, bounded by a serialized-byte cap.
///
/// The eval loop's incremental marking cursor ([`advance_marking`]) and the
/// dedup index only ever need recent events, so the tail is what serves
/// `events_from`. The public cursor contract is unchanged:
/// `len() == base_count + tail.len()`, and `events_from(idx)` slices the tail.
///
/// This bounds the memory used during hydration: every hydrated event flows
/// through [`Self::load_existing_event`], which immediately evicts down to the
/// cap, so the full log is never simultaneously resident — a multi-GB log
/// hydrates with only `tail_cap_bytes` (+ base marking/dedup) resident.
///
/// [`advance_marking`]: petri_application
pub struct MemoryEventStore {
    inner: RwLock<Inner>,
    /// Eviction threshold in serialized bytes. The tail is trimmed (oldest
    /// folded into base) until `tail_bytes <= cap` OR `tail.len() == 1`.
    tail_cap_bytes: usize,
}

struct Inner {
    /// Number of events folded into the base (their raw form is gone).
    base_count: usize,
    /// Projected marking of events `[0 .. base_count)`.
    base_marking: Marking,
    /// Dedup entries contributed by events `[0 .. base_count)`.
    base_dedup: DedupSeed,
    /// Hash of the event at position `base_count - 1` (chain tip of the base).
    /// `None` iff `base_count == 0`.
    base_last_hash: Option<String>,
    /// Verbatim recent events: positions `[base_count .. base_count+tail.len())`.
    tail: VecDeque<PersistedEvent>,
    /// Running serialized-byte size of `tail` (for cap enforcement).
    tail_bytes: usize,
    /// Next `.sequence` to assign on a *live* append. Tracked explicitly (not
    /// derived from `len()`) because the raw prefix is gone after eviction.
    next_sequence: u64,
    /// JetStream `stream_sequence` of the last event applied to this store via
    /// [`MemoryEventStore::load_existing_event_with_stream_seq`]. Recorded under
    /// the SAME write lock as the tail push so that `(marking, last_stream_seq)`
    /// captured by `snapshot_inputs_now` are ALWAYS coherent — they reflect the
    /// exact same prefix of applied events. This closes the hibernate snapshot
    /// skew race (MAJOR 2b): if the consumer applied an event in the window
    /// between two separate reads (marking vs. an external `last_stream_seq`
    /// cell), the snapshot's marking and resume point could disagree by one
    /// event, double-folding (or losing) it on the next wake. `0` until the
    /// first stream-seq-tagged apply (or a snapshot seed).
    last_applied_stream_seq: u64,
}

impl Inner {
    fn empty() -> Self {
        Self {
            base_count: 0,
            base_marking: Marking::new(),
            base_dedup: DedupSeed::new(),
            base_last_hash: None,
            tail: VecDeque::new(),
            tail_bytes: 0,
            next_sequence: 0,
            last_applied_stream_seq: 0,
        }
    }

    /// Storage-order count = base + tail.
    fn len(&self) -> usize {
        self.base_count + self.tail.len()
    }
}

/// Serialized byte size of an event (used for tail cap accounting). A
/// serialization failure (should not happen for valid events) counts as 0 —
/// the worst case is the cap being slightly under-counted, never a panic.
fn ser_len(e: &PersistedEvent) -> usize {
    serde_json::to_vec(e).map(|b| b.len()).unwrap_or(0)
}

/// Chain tip: hash of the last stored event (tail's last, else base tip).
fn chain_tip_hash(g: &Inner) -> Option<String> {
    g.tail
        .back()
        .map(|e| e.hash.clone())
        .or_else(|| g.base_last_hash.clone())
}

/// Push an event onto the tail, then evict the oldest tail events into the
/// base until the tail is under `cap` (but always keep at least one event in
/// the tail so the chain tip and the cursor base boundary stay well-defined).
fn push_tail(g: &mut Inner, e: PersistedEvent, cap: usize) {
    g.tail_bytes += ser_len(&e);
    g.tail.push_back(e);

    while g.tail_bytes > cap && g.tail.len() > 1 {
        let old = g.tail.pop_front().expect("len > 1");
        g.tail_bytes = g.tail_bytes.saturating_sub(ser_len(&old));
        // (1) fold into base marking — once the producing token is later
        //     consumed, its (possibly fat) payload leaves memory entirely.
        apply_event_to_marking(&mut g.base_marking, &old.event);
        // (2) fold into base dedup seed so the dedup window survives eviction.
        if let DomainEvent::TokenCreated {
            place_id,
            dedup_id: Some(id),
            ..
        } = &old.event
        {
            if !id.is_empty() {
                g.base_dedup
                    .insert((place_id.clone(), id.clone()), old.clone());
            }
        }
        // (3) advance base boundary + chain tip.
        g.base_count += 1;
        g.base_last_hash = Some(old.hash.clone());
    }
}

impl MemoryEventStore {
    /// Construct with the tail cap read from `PETRI_MAX_EVENT_TAIL_BYTES`
    /// (default [`DEFAULT_MAX_EVENT_TAIL_BYTES`]).
    pub fn new() -> Self {
        let cap = std::env::var("PETRI_MAX_EVENT_TAIL_BYTES")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(DEFAULT_MAX_EVENT_TAIL_BYTES);
        Self::with_tail_cap(cap)
    }

    /// Construct with an explicit tail cap in serialized bytes. Useful for
    /// deterministic tests of the eviction path.
    pub fn with_tail_cap(tail_cap_bytes: usize) -> Self {
        Self {
            inner: RwLock::new(Inner::empty()),
            tail_cap_bytes,
        }
    }

    /// Load an existing persisted event (e.g. from hydration or live consume).
    /// Does NOT recompute hash or sequence — trusts the input. Immediately
    /// evicts down to the cap, so hydration never holds the full log at once.
    ///
    /// Records the event's own `.sequence` as the applied JetStream
    /// `stream_sequence`. NATS callers that know the real `stream_sequence`
    /// (which differs from `.sequence` on multi-session streams) MUST use
    /// [`Self::load_existing_event_with_stream_seq`] so the hibernate snapshot's
    /// resume point is coherent with the marking. Non-NATS callers (tests,
    /// in-memory hydration) have no separate stream sequence, so `.sequence` is
    /// the correct value here.
    pub fn load_existing_event(&self, event: PersistedEvent) {
        let stream_seq = event.sequence;
        self.load_existing_event_with_stream_seq(event, stream_seq);
    }

    /// Load an existing persisted event AND record the JetStream
    /// `stream_sequence` it was delivered at — both under the SAME write lock.
    ///
    /// This atomic pairing is the fix for the hibernate snapshot skew race
    /// (MAJOR 2b): `snapshot_inputs_now` reads the marking and
    /// `last_applied_stream_seq` under one read lock, so they can never disagree
    /// by an event the consumer applied between two separate reads. The wake
    /// then resumes at `last_applied_stream_seq + 1`, exactly one past the last
    /// event already folded into the snapshot marking — no double-fold, no loss.
    pub fn load_existing_event_with_stream_seq(&self, event: PersistedEvent, stream_seq: u64) {
        let mut g = self.inner.write().unwrap();
        // Keep next_sequence ahead of any loaded event so a later live append
        // (rare on the NATS path) never reuses a sequence.
        g.next_sequence = g.next_sequence.max(event.sequence + 1);
        // Monotonic: a redelivery / out-of-order apply must never rewind the
        // recorded resume point below an already-applied stream position.
        g.last_applied_stream_seq = g.last_applied_stream_seq.max(stream_seq);
        push_tail(&mut g, event, self.tail_cap_bytes);
    }

    /// Chain tip: hash of the last stored event (tail's last, else base tip).
    pub fn last_hash(&self) -> Option<String> {
        chain_tip_hash(&self.inner.read().unwrap())
    }

    /// `(base_marking clone, tail clone)` — the inputs a marking-cache Miss
    /// path folds via `project_onto(&base, &tail)` to rebuild without the full
    /// history.
    pub fn base_and_tail(&self) -> (Marking, Vec<PersistedEvent>) {
        let g = self.inner.read().unwrap();
        (g.base_marking.clone(), g.tail.iter().cloned().collect())
    }

    /// Dedup seed = base dedup entries merged with a scan of the resident tail.
    pub fn dedup_seed_full(&self) -> DedupSeed {
        let g = self.inner.read().unwrap();
        let mut m = g.base_dedup.clone();
        for e in &g.tail {
            if let DomainEvent::TokenCreated {
                place_id,
                dedup_id: Some(id),
                ..
            } = &e.event
            {
                if !id.is_empty() {
                    m.insert((place_id.clone(), id.clone()), e.clone());
                }
            }
        }
        m
    }

    /// Capture the inputs for a hibernation snapshot from the live state: the
    /// FULL projected marking (base ⊕ tail), the FULL dedup seed, the chain tip,
    /// the storage-order count, and the next live sequence. Folds only the
    /// resident tail onto the already-projected base — never re-walks the
    /// dropped prefix.
    pub fn snapshot_inputs_now(&self) -> SnapshotInputs {
        let g = self.inner.read().unwrap();
        let mut marking = g.base_marking.clone();
        for p in &g.tail {
            apply_event_to_marking(&mut marking, &p.event);
        }
        // Inline the dedup-seed fold while holding the same lock (avoids a second
        // lock acquisition + clone of base_dedup).
        let mut dedup = g.base_dedup.clone();
        for e in &g.tail {
            if let DomainEvent::TokenCreated {
                place_id,
                dedup_id: Some(id),
                ..
            } = &e.event
            {
                if !id.is_empty() {
                    dedup.insert((place_id.clone(), id.clone()), e.clone());
                }
            }
        }
        SnapshotInputs {
            marking,
            dedup,
            last_hash: chain_tip_hash(&g),
            event_count: g.len() as u64,
            next_sequence: g.next_sequence,
            // Read under the SAME lock as `marking` above → always coherent with
            // the projected prefix (MAJOR 2b). The wake resumes at this + 1.
            last_stream_seq: g.last_applied_stream_seq,
            // No topology at this layer; the registry's `write_snapshot` fills it
            // from `service.get_topology()` before persisting.
            topology: None,
        }
    }

    /// Seed the store's base from a hibernation snapshot. The snapshot folds the
    /// ENTIRE pre-hibernate history into its base, so this installs
    /// `base_marking`/`base_dedup`/`base_last_hash`/`base_count`/`next_sequence`
    /// from the snapshot with an EMPTY tail (`base_count = event_count`, so
    /// `len()` resumes from the right storage position and the cursor base
    /// boundary is correct). The consumer then replays only the post-snapshot
    /// delta (`ByStartSequence(snapshot.last_stream_seq + 1)`) via
    /// `load_existing_event`, folding it onto this base.
    ///
    /// Only valid on a freshly-built (empty) store, which is exactly the wake
    /// path's invariant.
    pub fn seed_from_snapshot(&self, snapshot: &NetSnapshot) {
        let mut g = self.inner.write().unwrap();
        g.base_marking = snapshot.marking.clone();
        g.base_dedup = snapshot.dedup_seed();
        g.base_last_hash = snapshot.last_hash.clone();
        g.base_count = snapshot.event_count as usize;
        g.next_sequence = snapshot.next_sequence;
        // Restore the resume point so a re-hibernate before any post-wake apply
        // writes a snapshot that still points one past the original tip (rather
        // than rewinding to 0). The consumer also advances this as the delta
        // replays via `load_existing_event_with_stream_seq`.
        g.last_applied_stream_seq = snapshot.last_stream_seq;
        g.tail = VecDeque::new();
        g.tail_bytes = 0;
    }

    /// Approximate in-memory footprint of this store, read directly from the
    /// folded base + the running `tail_bytes` counter under one read lock.
    ///
    /// `tail_bytes` is already maintained on every push/evict, so only the base
    /// marking and base dedup index are serialized here. `base_dedup_*` reflect
    /// the PERMANENT (append-only) dedup index — the field to watch for the
    /// streaming-telemetry leak that historically OOM'd high-volume nets.
    pub fn memory_report_now(&self) -> EventStoreMemory {
        let g = self.inner.read().unwrap();
        EventStoreMemory {
            tail_bytes: g.tail_bytes,
            tail_events: g.tail.len(),
            base_marking_bytes: serde_json::to_vec(&g.base_marking)
                .map(|b| b.len())
                .unwrap_or(0),
            base_dedup_bytes: dedup_seed_bytes(&g.base_dedup),
            base_dedup_entries: g.base_dedup.len(),
            base_count: g.base_count,
            event_count: g.len(),
            total_bytes: 0,
        }
        .finalize()
    }
}

impl Default for MemoryEventStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl EventRepository for MemoryEventStore {
    async fn append(&self, event: DomainEvent) -> Result<PersistedEvent, EventStoreError> {
        let mut g = self.inner.write().unwrap();

        let sequence = g.next_sequence;
        let previous_hash = chain_tip_hash(&g);

        let persisted = PersistedEvent::new(sequence, event, previous_hash);
        g.next_sequence += 1;
        push_tail(&mut g, persisted.clone(), self.tail_cap_bytes);

        Ok(persisted)
    }

    async fn all_events(&self) -> Vec<PersistedEvent> {
        // Semantic note: this returns only the *materialized* (tail) events —
        // the base's raw events are gone (they live on the durable log).
        self.inner.read().unwrap().tail.iter().cloned().collect()
    }

    async fn events_since(&self, sequence: u64) -> Vec<PersistedEvent> {
        // Filters the tail by `.sequence` only. Base events are unavailable by
        // `.sequence`. This is acceptable — `events_since` is documented as
        // *not* cursor-safe and is not used on any path needing pre-base events.
        self.inner
            .read()
            .unwrap()
            .tail
            .iter()
            .filter(|e| e.sequence >= sequence)
            .cloned()
            .collect()
    }

    async fn reset(&self) {
        *self.inner.write().unwrap() = Inner::empty();
    }

    async fn current_sequence(&self) -> u64 {
        self.inner.read().unwrap().next_sequence
    }

    // Storage-order count and positional slice. These are the correct
    // primitives for incremental cache cursoring — `current_sequence` /
    // `events_since` filter on the `.sequence` field, which is unsafe when
    // the cache holds hydrated events whose numbering restarts at 0 across
    // sessions (multi-session NATS streams).

    async fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    async fn materialized_floor(&self) -> usize {
        self.inner.read().unwrap().base_count
    }

    async fn earliest_available_sequence(&self) -> Option<u64> {
        // The lowest sequence the in-memory store can still serve. Normally the
        // front-of-tail sequence. But when the tail is empty WHILE history
        // exists (`base_count > 0` — e.g. right after an empty-delta snapshot
        // wake, before any live event repopulates the tail), nothing is
        // resident, yet the prefix `[0 .. base_count)` lives only on the durable
        // log. Returning `None` there would make the GET-events / SSE backfill
        // under-report truncation (a client requesting `from_sequence = 0` would
        // see no `history_truncated` marker). Report `next_sequence` instead —
        // the in-memory window begins at the next event, so any earlier request
        // is correctly flagged truncated. Only a genuinely fresh store
        // (`base_count == 0`, tail empty) returns `None`.
        let g = self.inner.read().unwrap();
        match g.tail.front() {
            Some(e) => Some(e.sequence),
            None if g.base_count > 0 => Some(g.next_sequence),
            None => None,
        }
    }

    async fn events_from(&self, idx: usize) -> Vec<PersistedEvent> {
        let g = self.inner.read().unwrap();
        let base = g.base_count;
        if idx >= base + g.tail.len() {
            return Vec::new();
        }
        // For idx < base (a cursor into evicted territory) this clamps to the
        // tail start, which DROPS the evicted events `[idx .. base)`. That is a
        // lossy slice and is correct ONLY when no caller relies on those
        // dropped events. The sole correctness-sensitive consumer — the
        // marking-cache Stale path in `advance_marking` — guards against this
        // by checking `cached_idx >= materialized_floor()` (== `base_count`)
        // BEFORE calling `events_from`; when the cursor has fallen below the
        // floor (eviction advanced `base_count` past a stale cursor) it rebuilds
        // from `base ⊕ tail` instead of slicing. So this clamp is never reached
        // on a path that needs the dropped prefix; it stays only as a defensive
        // bound for incidental callers.
        let start_in_tail = idx.saturating_sub(base);
        g.tail.iter().skip(start_in_tail).cloned().collect()
    }

    async fn events_from_checked(&self, idx: usize) -> Option<Vec<PersistedEvent>> {
        // Check the floor AND slice under the SAME lock, so a concurrent
        // append+evict cannot advance `base_count` past `idx` between the guard
        // and the slice (the check-then-slice TOCTOU). If the cursor is below
        // the (current) base it would lossily clamp → return None so the caller
        // rebuilds from base ⊕ tail instead of silently dropping `[idx .. base)`.
        let g = self.inner.read().unwrap();
        let base = g.base_count;
        if idx < base {
            return None;
        }
        let start_in_tail = idx - base;
        Some(g.tail.iter().skip(start_in_tail).cloned().collect())
    }

    async fn marking_base(&self) -> (Marking, Vec<PersistedEvent>, u64) {
        // base_marking, resident tail, AND the storage-order extent
        // (base_count + tail.len()), all under ONE read lock so the cursor the
        // caller stores is coherent with the marking it projects.
        let g = self.inner.read().unwrap();
        let extent = (g.base_count + g.tail.len()) as u64;
        (
            g.base_marking.clone(),
            g.tail.iter().cloned().collect(),
            extent,
        )
    }

    async fn last_hash(&self) -> Option<String> {
        self.last_hash()
    }

    async fn write_cursor(&self) -> (u64, Option<String>) {
        // next_sequence + chain tip under ONE read lock (coherent pair).
        let g = self.inner.read().unwrap();
        (g.next_sequence, chain_tip_hash(&g))
    }

    async fn dedup_seed(&self) -> DedupSeed {
        self.dedup_seed_full()
    }

    async fn snapshot_inputs(&self) -> SnapshotInputs {
        self.snapshot_inputs_now()
    }

    async fn seed_from_snapshot(&self, snapshot: &NetSnapshot) {
        self.seed_from_snapshot(snapshot)
    }

    async fn memory_report(&self) -> EventStoreMemory {
        self.memory_report_now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_application::StateProjection;
    use petri_test_harness::prelude::*;

    #[rstest]
    #[tokio::test]
    async fn test_append_and_retrieve() {
        assert_append_and_retrieve(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_hash_chain_integrity() {
        assert_hash_chain_integrity(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_events_since() {
        assert_events_since(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_reset() {
        assert_reset(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_current_sequence() {
        assert_current_sequence(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_event_timestamps() {
        assert_event_timestamps(&MemoryEventStore::new()).await;
    }

    #[rstest]
    #[tokio::test]
    async fn test_append_after_reset() {
        assert_append_after_reset(&MemoryEventStore::new()).await;
    }

    use petri_domain::{DomainEvent, PersistedEvent, PlaceId, Token, TokenColor};

    fn token_created(seq: u64, place: &PlaceId) -> PersistedEvent {
        PersistedEvent::new(
            seq,
            DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Unit),
                place_id: place.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            },
            None,
        )
    }

    /// A token producer carrying a fat data payload (the OOM-driving shape).
    fn fat_effect(seq: u64, place: &PlaceId, payload_bytes: usize) -> PersistedEvent {
        let blob = "x".repeat(payload_bytes);
        let token = Token::new(TokenColor::Data(serde_json::json!({ "blob": blob })));
        PersistedEvent::new(
            seq,
            DomainEvent::EffectCompleted {
                transition_id: petri_domain::TransitionId::new(),
                transition_name: Some("fat".to_string()),
                consumed_tokens: vec![],
                produced_tokens: vec![(place.clone(), token)],
                effect_handler_id: "h".to_string(),
                effect_result: serde_json::json!({"status": "ok"}),
                read_tokens: vec![],
                process_step_started: None,
                process_step_completed: None,
            },
            Some("prev".to_string()),
        )
    }

    /// Index-based cursor (`len` + `events_from`) is safe when the cache
    /// holds hydrated events whose `.sequence` field restarts at 0 across
    /// sessions — the bug class behind `[[engine-loop-dup-seq]]`. The
    /// sequence-field-based pair (`current_sequence` / `events_since`) is
    /// *not* safe in that scenario; this test pins both behaviors. Run with a
    /// large cap so no eviction interferes with the cursor semantics.
    #[tokio::test]
    async fn cache_cursor_is_index_based_under_dup_sequences() {
        // Large cap: no eviction, full retention, original semantics hold.
        let store = MemoryEventStore::with_tail_cap(usize::MAX);
        let place = PlaceId::named("p");

        // Session 1: seq 0..=2
        store.load_existing_event(token_created(0, &place));
        store.load_existing_event(token_created(1, &place));
        store.load_existing_event(token_created(2, &place));
        // Session 2: seq 0..=2 again
        store.load_existing_event(token_created(0, &place));
        store.load_existing_event(token_created(1, &place));
        store.load_existing_event(token_created(2, &place));

        assert_eq!(store.len().await, 6);

        // `events_since(3)` filters by `.sequence >= 3` — finds NONE.
        let by_sequence_filter = store.events_since(3).await;
        assert_eq!(
            by_sequence_filter.len(),
            0,
            "events_since uses .sequence; with duplicate sequences it silently drops events"
        );

        // `events_from(3)` slices by storage position — finds the 3 events
        // appended after cursor 3, regardless of `.sequence`.
        let by_index_slice = store.events_from(3).await;
        assert_eq!(
            by_index_slice.len(),
            3,
            "events_from slices by index; safe under non-monotonic .sequence"
        );

        // Beyond end is a clamp, not a panic.
        assert_eq!(store.events_from(99).await.len(), 0);
    }

    /// Eviction keeps the resident set under the byte cap, while `len()` still
    /// reflects every event and the folded base ⊕ tail reproduces a full replay.
    #[tokio::test]
    async fn tail_evicts_under_byte_cap() {
        use crate::MarkingProjection;
        let place = PlaceId::named("p");
        // Cap below a single fat event's size forces eviction down to len 1.
        let store = MemoryEventStore::with_tail_cap(64 * 1024);

        let n = 8;
        let mut control = Vec::new();
        for i in 0..n {
            let e = fat_effect(i, &place, 256 * 1024); // 256 KiB payload each
            control.push(e.clone());
            store.load_existing_event(e);
        }

        // Storage count is all N; the materialized tail is fewer.
        assert_eq!(store.len().await, n as usize);
        assert!(
            store.all_events().await.len() < n as usize,
            "tail should be evicted below the full count"
        );
        let (base, tail) = store.base_and_tail();
        assert!(base_count_of(&store).await > 0, "some events folded to base");

        // project_onto(base, tail) == full replay of the N events.
        let proj = MarkingProjection::new();
        let folded = proj.project_onto(&base, &tail);
        let full = proj.project(&control);
        assert_eq!(folded.token_count(&place), full.token_count(&place));
        assert_eq!(full.token_count(&place), n as usize, "each fat token parked");
    }

    async fn base_count_of(store: &MemoryEventStore) -> usize {
        // len - materialized tail = folded base count.
        store.len().await - store.all_events().await.len()
    }

    /// After eviction: events_from(0) returns the tail, events_from(len)
    /// returns empty, events_from(base_count) returns the whole tail.
    #[tokio::test]
    async fn cursor_clamps_into_base() {
        let place = PlaceId::named("p");
        let store = MemoryEventStore::with_tail_cap(64 * 1024);
        for i in 0..8 {
            store.load_existing_event(fat_effect(i, &place, 256 * 1024));
        }

        let len = store.len().await;
        let tail_len = store.all_events().await.len();
        let base_count = len - tail_len;
        assert!(base_count > 0);

        // events_from(0) clamps into the base → returns the whole tail.
        assert_eq!(store.events_from(0).await.len(), tail_len);
        // events_from(base_count) → exactly the tail.
        assert_eq!(store.events_from(base_count).await.len(), tail_len);
        // events_from(len) → empty.
        assert_eq!(store.events_from(len).await.len(), 0);
    }

    /// `events_from_checked` rejects (returns `None`) any cursor below the
    /// materialized floor — the atomic check-and-slice that closes the
    /// advance_marking Stale-path TOCTOU. At/above the floor it returns the same
    /// slice as `events_from`.
    #[tokio::test]
    async fn events_from_checked_rejects_below_floor() {
        let place = PlaceId::named("p");
        let store = MemoryEventStore::with_tail_cap(64 * 1024);
        for i in 0..8 {
            store.load_existing_event(fat_effect(i, &place, 256 * 1024));
        }

        let len = store.len().await;
        let base_count = store.materialized_floor().await;
        let tail_len = store.all_events().await.len();
        assert!(base_count > 0, "eviction must have advanced the floor");

        // Below the floor → None (would lossily clamp).
        assert!(
            store.events_from_checked(0).await.is_none(),
            "cursor 0 < floor must be rejected"
        );
        assert!(
            store.events_from_checked(base_count - 1).await.is_none(),
            "cursor just below the floor must be rejected"
        );

        // At/above the floor → Some, matching events_from.
        let at_floor = store
            .events_from_checked(base_count)
            .await
            .expect("at floor must be Some");
        assert_eq!(at_floor.len(), tail_len);
        assert_eq!(
            store.events_from_checked(len).await.expect("len is Some").len(),
            0,
            "cursor at len → empty (not None: len >= floor)"
        );
    }

    /// Regression for the eviction-unsafe marking-cache Stale path.
    ///
    /// `advance_marking`'s Stale branch cursors by a stale `events.len()`
    /// snapshot. Between two calls, external listeners append+EVICT many
    /// events, advancing `base_count` (`materialized_floor`) PAST that cursor.
    /// The pre-fix Stale path then called `events_from(cached_idx)`, which the
    /// bounded store clamps to the tail start — SILENTLY DROPPING the evicted
    /// events `[cached_idx .. base_count)`. The cached marking then permanently
    /// diverged from `project(all_events)` (the eviction-induced re-fire of
    /// [[engine-loop-dup-seq]]).
    ///
    /// This drives the REAL `MemoryEventStore` through the REAL
    /// `advance_marking`: prime the cache at a low cursor, then append/evict
    /// enough fat events to push `base_count` past it, and assert the returned
    /// marking equals a full-replay projection. Pre-fix this asserted 3 vs 8;
    /// post-fix the `materialized_floor` guard rebuilds from base ⊕ tail → 8.
    #[tokio::test]
    async fn advance_marking_stale_path_is_eviction_safe() {
        use crate::MarkingProjection;
        use petri_application::{advance_marking, MarkingDelta};
        use std::sync::RwLock;

        let place = PlaceId::named("p");
        // 64 KiB tail cap vs 256 KiB fat events → aggressive eviction.
        let store = MemoryEventStore::with_tail_cap(64 * 1024);
        let proj = MarkingProjection::new();
        let cache: RwLock<Option<(u64, petri_domain::Marking)>> = RwLock::new(None);

        // -- Track the canonical full-replay history independently.
        let mut control: Vec<PersistedEvent> = Vec::new();

        // Prime the cache at a LOW cursor (cursor = 2): two small token-creates,
        // both resident (no eviction yet), then a first `advance_marking` seeds
        // the cache at (2, marking-with-2-tokens) via the Miss path.
        for i in 0..2u64 {
            let e = token_created(i, &place);
            control.push(e.clone());
            store.load_existing_event(e);
        }
        assert_eq!(base_count_of(&store).await, 0, "nothing evicted yet");
        let (primed, delta) = advance_marking(&store, &proj, &cache).await;
        assert!(matches!(delta, MarkingDelta::Rebuilt), "first call seeds cache");
        assert_eq!(primed.token_count(&place), 2);
        assert_eq!(
            cache.read().unwrap().as_ref().unwrap().0,
            2,
            "cursor primed at storage-order index 2"
        );

        // -- External-listener burst: append+evict six fat token-producers.
        // Each 256 KiB event exceeds the 64 KiB cap, so loading them folds the
        // two primed events AND the early fat events into the base — advancing
        // base_count FAR past the cached cursor of 2.
        for i in 2..8u64 {
            let e = fat_effect(i, &place, 256 * 1024);
            control.push(e.clone());
            store.load_existing_event(e);
        }
        let floor = store.materialized_floor().await;
        assert!(
            floor > 2,
            "eviction must advance base_count past the cached cursor of 2 (got {floor})"
        );

        // -- The Stale path now runs with cached_idx (2) < materialized_floor.
        // Pre-fix: events_from(2) clamps to the tail → drops [2..floor) → the
        // marking undercounts (3, not 8). Post-fix: the guard rebuilds from
        // base ⊕ tail → exactly the full-replay count.
        let (advanced, _) = advance_marking(&store, &proj, &cache).await;
        let full = proj.project(&control);
        assert_eq!(full.token_count(&place), 8, "8 tokens total were produced");
        assert_eq!(
            advanced.token_count(&place),
            full.token_count(&place),
            "advance_marking Stale path must equal full replay even after eviction \
             advanced base_count past the cached cursor (pre-fix: 3 != 8)"
        );

        // The cursor was re-seeded to the live storage-order count. Read the
        // guarded cursor into a local FIRST so the `RwLock` guard is dropped
        // before the `store.len().await` (no lock held across an await point).
        let cursor = cache.read().unwrap().as_ref().unwrap().0;
        assert_eq!(
            cursor,
            store.len().await as u64,
            "cursor re-synced to events.len() after the rebuild"
        );
    }

    /// FINDING 2 regression: `advance_marking` must store a cursor equal to the
    /// number of events it actually FOLDED, even when `events.len()` (read at the
    /// top of the call) LAGS the real resident extent because a concurrent append
    /// landed between the `len()` read and the data slice. A stale cursor would
    /// make the next Stale call re-fold the gap (over-fold → phantom tokens). The
    /// skew only arises under concurrency, so we simulate it with a store whose
    /// `len()` is frozen below its true resident extent.
    #[tokio::test]
    async fn advance_marking_stores_folded_extent_not_stale_len() {
        use crate::MarkingProjection;
        use petri_application::{advance_marking, EventRepository, MarkingDelta};
        use std::sync::RwLock;

        /// Wraps a full-retention store but reports a FROZEN, smaller `len()` —
        /// modelling `events.len()` read before a concurrent append, while
        /// `events_from_checked`/`marking_base` see the real (larger) extent.
        struct LaggingLenStore {
            inner: MemoryEventStore,
            frozen_len: usize,
        }

        #[async_trait::async_trait]
        impl EventRepository for LaggingLenStore {
            async fn append(
                &self,
                e: DomainEvent,
            ) -> Result<PersistedEvent, petri_application::EventStoreError> {
                self.inner.append(e).await
            }
            async fn all_events(&self) -> Vec<PersistedEvent> {
                self.inner.all_events().await
            }
            async fn events_since(&self, s: u64) -> Vec<PersistedEvent> {
                self.inner.events_since(s).await
            }
            async fn reset(&self) {
                self.inner.reset().await
            }
            async fn current_sequence(&self) -> u64 {
                self.inner.current_sequence().await
            }
            // The crux: len() lags the real resident extent.
            async fn len(&self) -> usize {
                self.frozen_len
            }
            async fn events_from_checked(&self, idx: usize) -> Option<Vec<PersistedEvent>> {
                self.inner.events_from_checked(idx).await
            }
            async fn marking_base(&self) -> (petri_domain::Marking, Vec<PersistedEvent>, u64) {
                self.inner.marking_base().await
            }
        }

        let place = PlaceId::named("p");
        let inner = MemoryEventStore::with_tail_cap(usize::MAX); // full retention
        for i in 0..5u64 {
            inner.load_existing_event(token_created(i, &place));
        }
        // 5 events resident, but len() reports only 2 (stale, pre-append read).
        let store = LaggingLenStore {
            inner,
            frozen_len: 2,
        };

        // Prime the cache at cursor 0 so the call takes the Stale → Applied branch.
        let cache: RwLock<Option<(u64, petri_domain::Marking)>> =
            RwLock::new(Some((0, petri_domain::Marking::new())));
        let proj = MarkingProjection::new();

        let (marking, delta) = advance_marking(&store, &proj, &cache).await;

        // It folded all 5 resident events (events_from_checked(0) returned them).
        assert_eq!(marking.token_count(&place), 5);
        assert!(matches!(delta, MarkingDelta::Applied(_)));

        // The stored cursor must be the EXTENT FOLDED (5), NOT the stale len() (2).
        // Storing 2 (pre-fix `current_idx`) would make the next Stale call slice
        // [2..5) again and re-fold those 3 events onto the already-complete
        // marking (over-fold → 8 tokens).
        let cursor = cache.read().unwrap().as_ref().unwrap().0;
        assert_eq!(
            cursor, 5,
            "Applied branch must store the folded extent (cached_idx + new_events.len()), \
             not the stale events.len() read at call entry"
        );
    }

    /// Minor regression: after an empty-delta snapshot wake the tail is empty but
    /// history exists (`base_count > 0`). `earliest_available_sequence()` must
    /// report the truncation floor (so GET-events/SSE flag `history_truncated`),
    /// not `None` — which would under-report and let a reconnecting client believe
    /// it is caught up while the evicted prefix is absent from the in-memory view.
    #[tokio::test]
    async fn earliest_available_sequence_reports_floor_when_tail_empty_with_history() {
        use petri_application::EventRepository;

        let place = PlaceId::named("p");
        let src = MemoryEventStore::with_tail_cap(64 * 1024);
        for i in 0..6u64 {
            src.load_existing_event(fat_effect(i, &place, 256 * 1024));
        }
        let snapshot = src.snapshot_inputs_now().into_snapshot();
        assert!(snapshot.event_count > 0);

        // Seed a fresh store → empty tail, base_count = event_count.
        let woken = MemoryEventStore::with_tail_cap(64 * 1024);
        woken.seed_from_snapshot(&snapshot);
        assert_eq!(woken.all_events().await.len(), 0, "tail empty after wake");
        assert!(
            woken.materialized_floor().await > 0,
            "history exists in base"
        );

        // Must NOT be None — report next_sequence as the truncation floor.
        assert_eq!(
            woken.earliest_available_sequence().await,
            Some(snapshot.next_sequence),
            "empty-tail-with-history must flag truncation, not under-report None"
        );

        // A genuinely fresh store (no history) still returns None.
        let fresh = MemoryEventStore::with_tail_cap(64 * 1024);
        assert_eq!(fresh.earliest_available_sequence().await, None);
    }

    /// The chain tip survives eviction, and a later append links to it.
    #[tokio::test]
    async fn hash_tip_survives_eviction() {
        let place = PlaceId::named("p");
        let store = MemoryEventStore::with_tail_cap(64 * 1024);

        let mut last_loaded_hash = None;
        for i in 0..8 {
            let e = fat_effect(i, &place, 256 * 1024);
            last_loaded_hash = Some(e.hash.clone());
            store.load_existing_event(e);
        }
        // last_hash == hash of the most-recently-stored event.
        assert_eq!(store.last_hash(), last_loaded_hash);

        // A fresh append links its previous_hash to the current tip.
        let tip_before = store.last_hash();
        let appended = store
            .append(DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Unit),
                place_id: place.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            })
            .await
            .unwrap();
        assert_eq!(appended.previous_hash, tip_before);
    }

    /// A `TokenCreated{dedup_id}` evicted into the base is still found via
    /// `dedup_seed`.
    #[tokio::test]
    async fn dedup_seed_survives_eviction() {
        let place = PlaceId::named("p");
        let store = MemoryEventStore::with_tail_cap(64 * 1024);

        // One small dedup-bearing event first, then fat events to evict it.
        let dedup_evt = PersistedEvent::new(
            0,
            DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Unit),
                place_id: place.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: Some("dedup-key-1".to_string()),
            },
            None,
        );
        store.load_existing_event(dedup_evt.clone());
        for i in 1..8 {
            store.load_existing_event(fat_effect(i, &place, 256 * 1024));
        }

        // The dedup event must have been evicted into the base.
        assert!(store.all_events().await.iter().all(|e| {
            !matches!(&e.event, DomainEvent::TokenCreated { dedup_id: Some(id), .. } if id == "dedup-key-1")
        }));

        // ...yet the dedup seed still contains it.
        let seed = store.dedup_seed().await;
        assert!(
            seed.contains_key(&(place.clone(), "dedup-key-1".to_string())),
            "evicted dedup key must survive in the base seed"
        );
    }

    /// A streaming/ephemeral `TokenCreated` carrying a fat payload but NO
    /// `dedup_id` — the shape executor progress/metric/log emits take after the
    /// watcher fix (they set `Nats-Msg-Id` for the 120s transport window but
    /// leave `ExternalSignal.dedup_id = None`).
    fn streaming_token(seq: u64, place: &PlaceId, payload_bytes: usize) -> PersistedEvent {
        let blob = "s".repeat(payload_bytes);
        PersistedEvent::new(
            seq,
            DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Data(serde_json::json!({ "blob": blob }))),
                place_id: place.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: Some("exec-42".to_string()),
                dedup_id: None,
            },
            Some("prev".to_string()),
        )
    }

    /// Regression for the streaming-dedup memory leak: a high-volume stream of
    /// EPHEMERAL `TokenCreated` events (unique-per-fire, `dedup_id = None`) must
    /// leave `base_dedup`/`dedup_seed` BOUNDED (≈0), even though every event is
    /// evicted into the base. The eviction fold in `push_tail` only retains
    /// `Some(dedup_id)`, so `None`-keyed streaming emits never accumulate — the
    /// permanent dedup footprint stays O(1) regardless of stream length, while
    /// the tokens themselves still park correctly (sink/marking unaffected).
    #[tokio::test]
    async fn ephemeral_streaming_tokens_keep_dedup_seed_bounded() {
        use crate::MarkingProjection;
        use petri_application::StateProjection;

        let place = PlaceId::named("metric_log");
        // Tight cap so essentially every event is evicted into the base.
        let store = MemoryEventStore::with_tail_cap(64 * 1024);

        // One deterministic ONE-SHOT dedup-bearing event up front: it MUST
        // survive eviction in the seed (contrast — proves we only suppress the
        // ephemeral ones, not all dedup).
        store.load_existing_event(PersistedEvent::new(
            0,
            DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Unit),
                place_id: place.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: Some("exec-42-output-result".to_string()),
            },
            None,
        ));

        // N=1000 ephemeral streaming emits, each ~2 KiB, forcing eviction.
        let n: u64 = 1000;
        for i in 1..=n {
            store.load_existing_event(streaming_token(i, &place, 2 * 1024));
        }

        // len() still reports every event (cursor contract intact).
        assert_eq!(
            store.len().await as u64,
            n + 1,
            "len() must report all N+1 events"
        );

        // ---- The bound: the dedup seed holds ONLY the one-shot key. ----
        let seed = store.dedup_seed().await;
        assert!(
            seed.contains_key(&(place.clone(), "exec-42-output-result".to_string())),
            "the one-shot deterministic dedup key must survive eviction"
        );
        assert_eq!(
            seed.len(),
            1,
            "dedup seed must stay O(1): {n} ephemeral streaming emits must NOT \
             accumulate permanent dedup entries (got {} entries)",
            seed.len()
        );

        // ---- Sink/marking behavior is unaffected: all N+1 tokens parked. ----
        let (base, tail) = store.base_and_tail();
        let folded = MarkingProjection::new().project_onto(&base, &tail);
        assert_eq!(
            folded.token_count(&place),
            (n + 1) as usize,
            "every streaming token must still be produced/parked at the sink place"
        );
    }

    /// PART C invariant (1)+(3): a snapshot captured from a live store, then
    /// seeded into a fresh store, reproduces the exact marking and chain tip —
    /// and a `TokenCreated` dedup key survives the round-trip (invariant 2).
    #[tokio::test]
    async fn snapshot_round_trip_preserves_marking_dedup_and_hash() {
        use crate::MarkingProjection;
        use petri_application::StateProjection;

        let place = PlaceId::named("p");
        let src = MemoryEventStore::with_tail_cap(64 * 1024);

        // A mix: a dedup-bearing TokenCreated + several fat parked producers
        // (forcing eviction so the snapshot is built from base ⊕ tail).
        let dedup_evt = PersistedEvent::new(
            0,
            DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Unit),
                place_id: place.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: Some("dk-1".to_string()),
            },
            None,
        );
        src.load_existing_event(dedup_evt);
        for i in 1..8 {
            src.load_existing_event(fat_effect(i, &place, 256 * 1024));
        }

        // Capture inputs → snapshot. last_stream_seq is now store-tracked: the
        // last loaded event (seq 7) sets it via load_existing_event.
        let inputs = src.snapshot_inputs_now();
        let expected_count = inputs.event_count;
        let expected_hash = inputs.last_hash.clone();
        assert_eq!(
            inputs.last_stream_seq, 7,
            "snapshot last_stream_seq must equal the last applied event's stream seq"
        );
        let snapshot = inputs.into_snapshot();
        assert_eq!(snapshot.last_stream_seq, 7);

        // Seed a FRESH store from the snapshot.
        let woken = MemoryEventStore::with_tail_cap(64 * 1024);
        woken.seed_from_snapshot(&snapshot);

        // (1)+(3) marking: woken base ⊕ (empty tail) == source full marking.
        let proj = MarkingProjection::new();
        let (wbase, wtail) = woken.base_and_tail();
        let woken_marking = proj.project_onto(&wbase, &wtail);
        let (sbase, stail) = src.base_and_tail();
        let src_marking = proj.project_onto(&sbase, &stail);
        assert_eq!(
            woken_marking.token_count(&place),
            src_marking.token_count(&place),
            "woken marking must equal the source marking"
        );
        assert_eq!(woken_marking.token_count(&place), 8, "all 8 tokens parked");

        // len() resumes from the right storage position.
        assert_eq!(woken.len().await as u64, expected_count);

        // (1) chain tip survives.
        assert_eq!(woken.last_hash(), expected_hash);

        // (2) dedup key survives the snapshot round-trip.
        let seed = woken.dedup_seed().await;
        assert!(
            seed.contains_key(&(place.clone(), "dk-1".to_string())),
            "dedup key must survive the snapshot round-trip"
        );
    }

    /// HEADLINE OOM-fix proof. Hydrates a synthetic high-volume log — N=5000
    /// fat parked producers, each carrying a ~10 KB payload (the crawl shape
    /// that OOM'd a 1 GB engine) — through the store's `load_existing_event`
    /// hydration path, exactly as the NATS consumer does on wake.
    ///
    /// Asserts the store's RETAINED in-memory footprint is bounded by the tail
    /// cap (O(cap)), NOT by the log length (O(N)):
    ///   - retained tail bytes ≈ cap (not N × payload),
    ///   - retained tail event count ≪ N,
    /// while `len()` still reports all N and the folded base ⊕ tail marking
    /// reproduces the correct token placement (all N tokens parked).
    #[tokio::test]
    async fn hydrating_huge_log_keeps_memory_bounded_by_cap() {
        use crate::MarkingProjection;
        use petri_application::StateProjection;

        let place = PlaceId::named("p_data");

        // ~10 KB payload per event. With N=5000 the full log is ~50 MB+ of
        // payload; a naive Vec-of-all-events store would hold all of it.
        let payload_bytes = 10 * 1024;
        let n: u64 = 5000;

        // Cap at 1 MiB: far below the full-log footprint, so the bound is
        // meaningful. A single fat event is ~10 KB, so the resident tail
        // should settle around ~100 events (1 MiB / 10 KB), never N.
        let cap = 1024 * 1024;
        let store = MemoryEventStore::with_tail_cap(cap);

        // One representative event's serialized size, for the bound math.
        let one = fat_effect(0, &place, payload_bytes);
        let one_ser = ser_len(&one);

        // Hydrate N fat events one-at-a-time, as the consumer does. We do NOT
        // retain a Vec of all of them here — we reconstruct the expected
        // marking analytically (each event parks exactly one token at `place`).
        for i in 0..n {
            store.load_existing_event(fat_effect(i, &place, payload_bytes));
        }

        // ---- The bound: retained memory is O(cap), not O(N). ----
        let retained_tail = store.all_events().await.len();
        let retained_bytes: usize = store
            .all_events()
            .await
            .iter()
            .map(ser_len)
            .sum();

        // len() still reports every event (cursor contract intact).
        assert_eq!(store.len().await as u64, n, "len() must report all N events");

        // The naive footprint we are NOT paying.
        let naive_bytes = (n as usize) * one_ser;

        // Resident tail bytes are within one event of the cap — O(cap).
        assert!(
            retained_bytes <= cap + one_ser,
            "retained tail bytes {retained_bytes} must be bounded by cap {cap} (+1 event slack {one_ser}); \
             naive store would hold ~{naive_bytes} bytes"
        );

        // Resident tail event count is O(cap/event), drastically below N.
        let expected_tail_ceiling = cap / one_ser + 2; // cap budget + slack
        assert!(
            retained_tail <= expected_tail_ceiling,
            "retained tail count {retained_tail} must be ≤ ~{expected_tail_ceiling} (cap/event), not N={n}"
        );
        assert!(
            (retained_tail as u64) < n / 10,
            "retained tail count {retained_tail} must be a small fraction of N={n}"
        );

        // Concrete numbers, surfaced in test output.
        println!(
            "OOM-bound proof: N={n}, payload={payload_bytes}B, per-event ser={one_ser}B, \
             cap={cap}B  ->  retained tail = {retained_tail} events / {retained_bytes}B \
             (naive would be {n} events / {naive_bytes}B)"
        );

        // ---- Correctness: folded base ⊕ tail == full replay. ----
        let (base, tail) = store.base_and_tail();
        let proj = MarkingProjection::new();
        let folded = proj.project_onto(&base, &tail);
        assert_eq!(
            folded.token_count(&place),
            n as usize,
            "every one of the N fat tokens must be parked in the folded marking"
        );

        // Sanity: the marking-cache Miss inputs (base, tail) themselves are
        // bounded — the base is a single folded Marking, the tail is O(cap).
        assert!(
            tail.len() <= expected_tail_ceiling,
            "base_and_tail() tail must also be O(cap)"
        );
    }

    /// PART C invariant (1): an `append` after a snapshot restore chains its
    /// `previous_hash` to the snapshot's chain tip — exactly as a full replay
    /// would, so the hash chain is byte-identical across the wake.
    #[tokio::test]
    async fn append_after_restore_chains_valid_previous_hash() {
        let place = PlaceId::named("p");
        let src = MemoryEventStore::with_tail_cap(64 * 1024);
        for i in 0..6 {
            src.load_existing_event(fat_effect(i, &place, 256 * 1024));
        }
        let snapshot = src.snapshot_inputs_now().into_snapshot();
        let tip = snapshot.last_hash.clone();
        assert!(tip.is_some(), "non-empty history must have a chain tip");

        let woken = MemoryEventStore::with_tail_cap(64 * 1024);
        woken.seed_from_snapshot(&snapshot);

        // First post-wake append links to the snapshot tip and continues the
        // sequence from the seeded next_sequence.
        let next_seq_before = woken.current_sequence().await;
        let appended = woken
            .append(DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Unit),
                place_id: place.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: None,
            })
            .await
            .unwrap();
        assert_eq!(
            appended.previous_hash, tip,
            "post-restore append must chain to the snapshot tip"
        );
        assert_eq!(
            appended.sequence, next_seq_before,
            "post-restore append must continue from the seeded next_sequence"
        );
        // The new event is now the chain tip.
        assert_eq!(woken.last_hash(), Some(appended.hash.clone()));
    }

    /// `memory_report` accounts the three growing regions and stays a faithful
    /// leak detector: under a high-volume stream of EPHEMERAL (no-dedup) fat
    /// tokens the tail stays bounded by the cap and the permanent dedup index
    /// stays O(1), while the folded base marking is where the parked-token bytes
    /// accumulate. This is exactly the signal an operator needs to tell "marking
    /// growth" from "the dedup leak" before an OOM.
    #[tokio::test]
    async fn memory_report_accounts_regions_and_flags_dedup_bound() {
        let place = PlaceId::named("metric_log");
        let cap = 64 * 1024;
        let store = MemoryEventStore::with_tail_cap(cap);

        // One one-shot dedup-bearing event (must persist in the permanent index).
        store.load_existing_event(PersistedEvent::new(
            0,
            DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Unit),
                place_id: place.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: Some("one-shot".to_string()),
            },
            None,
        ));

        // N ephemeral fat streaming emits (dedup_id = None) → evicted into base.
        let n: u64 = 500;
        for i in 1..=n {
            store.load_existing_event(streaming_token(i, &place, 2 * 1024));
        }

        let report = store.memory_report_now();

        // Cursor contract: every event is counted.
        assert_eq!(report.event_count as u64, n + 1);
        // Tail is bounded by the cap (+ at most one event of slack).
        assert!(
            report.tail_bytes <= cap + 4 * 1024,
            "tail_bytes {} must stay near the cap {cap}",
            report.tail_bytes
        );
        // The permanent dedup index holds ONLY the one-shot key — not the N
        // ephemerals. This is the leak-detection signal.
        assert_eq!(
            report.base_dedup_entries, 1,
            "ephemeral streaming emits must not accumulate in the dedup index"
        );
        assert!(report.base_dedup_bytes > 0, "the one-shot entry has bytes");
        // The parked fat tokens live in the folded base marking.
        assert!(
            report.base_marking_bytes > report.base_dedup_bytes,
            "base marking ({}) should dominate the dedup index ({}) here",
            report.base_marking_bytes,
            report.base_dedup_bytes
        );
        // total_bytes is the sum of the three serialized regions.
        assert_eq!(
            report.total_bytes,
            report.tail_bytes + report.base_marking_bytes + report.base_dedup_bytes
        );
    }
}
