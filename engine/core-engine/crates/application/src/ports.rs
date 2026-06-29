use std::collections::{HashMap, VecDeque};

use petri_domain::{
    apply_event_to_marking, DomainEvent, Marking, PersistedEvent, PetriNet, PlaceId, TransitionId,
};
use thiserror::Error;

/// Max distinct one-shot `(place,dedup_id)` entries retained for redelivery
/// suppression before the OLDEST is FIFO-evicted.
///
/// **Why a bounded FIFO window is correct (the retention argument).** A
/// redeliverable JetStream message is, at any instant, one of the UNACKED set:
/// the ingress listeners ack only AFTER the resulting `TokenCreated` is durable
/// (`nats/src/message_loop.rs`, ack-after-persist). So a one-shot id cannot stay
/// unacked while K *newer* one-shot ids are applied past it — that would require
/// the engine to apply K more events without acking this one, impossible unless
/// it crashed (in which case those K were never applied). During hibernation no
/// applies happen, so the ring is frozen; the unacked-at-hibernate set
/// redelivers on wake and is still entirely within the most-recent-K window
/// restored from the snapshot. The redeliverable horizon is therefore bounded by
/// the summed `max_ack_pending` of the ingress consumers (~6 consumers ×
/// server-default 1000 ≈ 6000 worst case). `16384` is ~2.7× that nominal ceiling
/// and orders of magnitude over the *real* applied-but-unacked horizon (a handful
/// under ack-after-persist). Raise via `PETRI_MAX_DEDUP_ENTRIES` if
/// `max_ack_pending` is raised on ingress consumers.
///
/// Streaming emits pass `dedup_id = None` (Step-1 carve-out) and are never
/// indexed, so they never consume a slot.
pub const DEFAULT_MAX_DEDUP_ENTRIES: usize = 16384;

/// Read the dedup-ring capacity from `PETRI_MAX_DEDUP_ENTRIES`, falling back to
/// [`DEFAULT_MAX_DEDUP_ENTRIES`]. Mirrors the inline `PETRI_MAX_EVENT_TAIL_BYTES`
/// parse in the infrastructure store; both the live `DedupIndex` and the store's
/// `base_dedup` read their cap through this single point, so the two windows
/// cannot drift in policy.
pub fn default_max_dedup_entries() -> usize {
    std::env::var("PETRI_MAX_DEDUP_ENTRIES")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_DEDUP_ENTRIES)
}

/// Bounded, insertion-ordered FIFO map of `(place_id, dedup_id)` → originating
/// `TokenCreated` event. Backs BOTH the live idempotency index
/// ([`crate::idempotency_index::DedupIndex`]) AND the event store's snapshot
/// seed, so bounding the *type* bounds both with no divergence: on insert beyond
/// `cap` the OLDEST (first-inserted, still-present) entry is evicted.
///
/// Retention only — the content KEY `(place_id, dedup_id)` is unchanged, so
/// relay/bridge-replay/human cross-consumer dedup all keep working; only the
/// window is bounded. See [`DEFAULT_MAX_DEDUP_ENTRIES`] for the headroom
/// argument that proves the most-recent-K window covers every redeliverable
/// message.
#[derive(Debug, Clone)]
pub struct BoundedDedup {
    map: HashMap<(PlaceId, String), PersistedEvent>,
    /// First-insertion order; each present key appears exactly once. Always the
    /// key set of `map`, so `pop_front` is the genuinely-oldest live key.
    order: VecDeque<(PlaceId, String)>,
    cap: usize,
}

impl BoundedDedup {
    /// New ring at the env-derived default cap ([`default_max_dedup_entries`]).
    pub fn new() -> Self {
        Self::with_cap(default_max_dedup_entries())
    }

    /// New ring with an explicit capacity (deterministic eviction tests).
    ///
    /// `cap` is clamped to `>= 1`: a zero cap would evict every key on the same
    /// `insert` call that added it, silently disabling dedup and re-admitting
    /// duplicate `TokenCreated` events — a misconfiguration footgun via
    /// `PETRI_MAX_DEDUP_ENTRIES=0`, not a supported "disable" switch.
    pub fn with_cap(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap: cap.max(1),
        }
    }

    /// Insert `key → event`, FIFO-evicting the oldest entry while over capacity.
    /// Re-inserting an already-present key updates the value and leaves its FIFO
    /// position unchanged (no double-counting in `order`).
    pub fn insert(&mut self, key: (PlaceId, String), event: PersistedEvent) {
        if self.map.insert(key.clone(), event).is_none() {
            self.order.push_back(key);
            while self.map.len() > self.cap {
                match self.order.pop_front() {
                    Some(old) => {
                        self.map.remove(&old);
                    }
                    None => break,
                }
            }
        }
    }

    /// Lookup by content key.
    pub fn get(&self, key: &(PlaceId, String)) -> Option<&PersistedEvent> {
        self.map.get(key)
    }

    /// Whether the content key is in the current window.
    pub fn contains_key(&self, key: &(PlaceId, String)) -> bool {
        self.map.contains_key(key)
    }

    /// Number of entries currently retained (`<= cap`).
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Iterate the retained `(key, event)` pairs (unordered).
    pub fn iter(&self) -> impl Iterator<Item = (&(PlaceId, String), &PersistedEvent)> {
        self.map.iter()
    }
}

impl Default for BoundedDedup {
    fn default() -> Self {
        Self::new()
    }
}

/// `(place_id, dedup_id)` → originating `TokenCreated` event, retained in a
/// bounded FIFO window ([`BoundedDedup`]). The idempotency index
/// ([`crate::idempotency_index::DedupIndex`]) is seeded from this; a bounded
/// event store contributes the entries of evicted events here so the dedup
/// window survives prefix eviction.
pub type DedupSeed = BoundedDedup;

/// Error type for event store operations.
#[derive(Error, Debug, Clone)]
pub enum EventStoreError {
    #[error("Failed to persist event: {0}")]
    PersistFailed(String),
    #[error("Timeout waiting for event persistence")]
    Timeout,
}

/// Port for event storage (outbound).
/// Implementations provide persistence for the event log.
// `len` here is the storage-order event count used as a projection cursor, not a
// collection length; an `is_empty` companion would be meaningless for the trait.
#[allow(clippy::len_without_is_empty)]
#[async_trait::async_trait]
pub trait EventRepository: Send + Sync {
    /// Append a new event to the log.
    /// Returns the persisted event with sequence number and hash.
    /// May fail if the underlying store is unavailable (e.g., NATS down).
    async fn append(&self, event: DomainEvent) -> Result<PersistedEvent, EventStoreError>;

    /// Get all events in storage order.
    async fn all_events(&self) -> Vec<PersistedEvent>;

    /// Get events whose `.sequence` field is `>= sequence`.
    ///
    /// Filters by the *content* of `PersistedEvent.sequence`. This is **not**
    /// safe to use for incremental cache cursoring when the log can contain
    /// events with non-monotonic `.sequence` (e.g. hydrated old sessions whose
    /// numbering overlaps with the current run). Prefer
    /// [`events_from`](Self::events_from) for cache/cursor use cases.
    async fn events_since(&self, sequence: u64) -> Vec<PersistedEvent>;

    /// Clear all events (for testing/reset).
    async fn reset(&self);

    /// Get the current sequence number that the next live append will use.
    ///
    /// Implementations backed only by an in-memory `Vec` return `len()` here,
    /// which coincides with "next sequence" only when sequences are monotonic
    /// 0..len. For cache/cursor logic, prefer [`len`](Self::len) which is
    /// always the storage-order count.
    async fn current_sequence(&self) -> u64;

    /// Number of events currently in the log (storage-order count).
    ///
    /// This is the correct cursor for incremental projection: pair it with
    /// [`events_from`](Self::events_from) to slice the events appended since
    /// a remembered position. It is always monotonic w.r.t. live appends,
    /// even if the cache was hydrated with events carrying overlapping
    /// `.sequence` fields.
    ///
    /// Default goes through `all_events().len()` — correct for any impl,
    /// but allocates. Override with a direct length read where possible.
    async fn len(&self) -> usize {
        self.all_events().await.len()
    }

    /// The lowest storage-order index that is still materialized verbatim.
    ///
    /// For a full-retention store this is always `0` — every event remains
    /// sliceable by position. A bounded store returns its `base_count`: events
    /// at positions `[0 .. materialized_floor())` have been **evicted** (folded
    /// into the base marking) and are no longer returned by
    /// [`events_from`](Self::events_from).
    ///
    /// This is the guard the marking-cache Stale path needs: a remembered
    /// cursor `i` is only safe to slice with `events_from(i)` while
    /// `i >= materialized_floor()`. If eviction has advanced the floor past a
    /// stale cursor (external listeners append+evict thousands of tokens
    /// between eval cycles), `events_from(i)` would silently clamp to the tail
    /// start and DROP events `[i .. materialized_floor())`, drifting the cached
    /// marking away from `f(events)` (the eviction-induced re-fire of
    /// [[engine-loop-dup-seq]]). The Stale path detects `i < materialized_floor()`
    /// and rebuilds from `base ⊕ tail` (Miss semantics) instead.
    ///
    /// Default returns `0` (full-retention safe). The bounded `MemoryEventStore`
    /// overrides it to return `base_count`.
    async fn materialized_floor(&self) -> usize {
        0
    }

    /// The `.sequence` of the earliest event still materialized verbatim, or
    /// `None` if the log is empty.
    ///
    /// For a full-retention store this is `0` whenever any event exists (the
    /// genesis event is always resident). For a **bounded** store whose prefix
    /// has been evicted, this is the `.sequence` of the oldest event still in
    /// the resident tail — i.e. the lowest sequence the in-memory view can
    /// serve. History/inspection endpoints use it to tell a client that a
    /// requested `from_sequence` falls below what memory holds (the durable
    /// NATS log still has the evicted prefix), so the response can flag
    /// `history_truncated` + surface this as `earliest_available_sequence`.
    ///
    /// Default reads `all_events().first()` — correct for any impl. Bounded
    /// stores override it to avoid materializing the tail just to read one
    /// sequence number.
    async fn earliest_available_sequence(&self) -> Option<u64> {
        self.all_events().await.first().map(|e| e.sequence)
    }

    /// Slice the log from the given storage-order index to the end.
    ///
    /// Unlike [`events_since`](Self::events_since) this filters by *position*
    /// in the log, not by the `.sequence` field. Use this — paired with
    /// [`len`](Self::len) — to drive incremental marking cache updates: a
    /// remembered index `i` plus `events_from(i)` always yields exactly the
    /// events appended after `i`, regardless of whether their `.sequence`
    /// values overlap with earlier hydrated events.
    ///
    /// Default slices `all_events()` — correct for any impl, but copies the
    /// full log. Override with a direct positional slice where possible.
    async fn events_from(&self, idx: usize) -> Vec<PersistedEvent> {
        let all = self.all_events().await;
        let start = idx.min(all.len());
        all[start..].to_vec()
    }

    /// Like [`events_from`](Self::events_from) but **atomically** rejects a
    /// cursor that has fallen below the materialized floor.
    ///
    /// Returns `Some(slice)` when `idx >= materialized_floor()` (the cursor
    /// still points into resident, sliceable territory), or `None` when
    /// `idx < materialized_floor()` (eviction advanced the floor past the
    /// cursor — slicing would lossily clamp to the tail and DROP events
    /// `[idx .. floor)`).
    ///
    /// The floor-check and the slice happen **under one lock** in the bounded
    /// store, closing the check-then-slice TOCTOU window: with two separate
    /// calls (`materialized_floor()` then `events_from()`) a concurrent
    /// append+evict between them could advance the floor past a cursor that
    /// passed the guard, and the subsequent `events_from` would silently drop
    /// the just-evicted events — the exact eviction-induced divergence
    /// ([[engine-loop-dup-seq]]) the guard exists to prevent. The marking-cache
    /// Stale path uses this so the decision (slice vs. rebuild) is coherent with
    /// the data it acts on.
    ///
    /// Default delegates to `events_from` because a full-retention store never
    /// evicts (`materialized_floor() == 0` always, so `idx >= 0` is always
    /// `Some`). Bounded stores override it to perform the check + slice under
    /// their single inner lock.
    async fn events_from_checked(&self, idx: usize) -> Option<Vec<PersistedEvent>> {
        Some(self.events_from(idx).await)
    }

    /// Marking-rebuild inputs: a base marking summarizing events that are no
    /// longer materialized verbatim, plus the still-resident events to fold on
    /// top. The marking-cache Miss path uses this to rebuild without holding
    /// the full history in memory.
    ///
    /// `project_onto(base, tail)` must equal `project(all_events_ever)`.
    ///
    /// The third element is the **storage-order extent** these inputs cover
    /// (`base_count + tail.len()`), read under the SAME lock as `base`/`tail`.
    /// The marking-cache Miss/Rebuild paths store it verbatim as the new cursor
    /// so the cursor always equals the number of events actually folded into the
    /// returned marking — using the call-site `events.len()` instead would tear
    /// if a concurrent append landed between the `len()` read and this one,
    /// leaving a stale cursor that re-folds events on the next call (the
    /// eviction-induced over-fold variant of [[engine-loop-dup-seq]]).
    ///
    /// Default returns `(empty marking, all_events(), all_events().len())` —
    /// correct for any full-retention impl, which keeps the whole history
    /// resident. A bounded store overrides this to return its folded base plus
    /// the resident tail and the coherent extent.
    async fn marking_base(&self) -> (Marking, Vec<PersistedEvent>, u64) {
        let all = self.all_events().await;
        let extent = all.len() as u64;
        (Marking::new(), all, extent)
    }

    /// Hash of the last stored event (the chain tip), or `None` if empty.
    ///
    /// Default reads `all_events().last()` — correct for any impl. A bounded
    /// store overrides this so it need not materialize the tail to read one
    /// hash, and so the tip survives prefix eviction.
    async fn last_hash(&self) -> Option<String> {
        self.all_events().await.last().map(|e| e.hash.clone())
    }

    /// Capture the inputs for a hibernation snapshot: the FULL projected
    /// marking, the FULL dedup seed, the chain tip, the storage-order event
    /// count, and the next live sequence. The caller (the registry's hibernate
    /// hook) pairs this with the consumer-tracked `last_stream_seq` to build a
    /// [`crate::net_snapshot::NetSnapshot`].
    ///
    /// Default folds `all_events()` for the marking — correct for any impl. A
    /// bounded store overrides this so it folds its already-projected base
    /// marking plus only the resident tail (never re-walking the dropped
    /// prefix).
    async fn snapshot_inputs(&self) -> crate::net_snapshot::SnapshotInputs {
        let (base, tail, _extent) = self.marking_base().await;
        let mut marking = base;
        for p in &tail {
            apply_event_to_marking(&mut marking, &p.event);
        }
        crate::net_snapshot::SnapshotInputs {
            marking,
            dedup: self.dedup_seed().await,
            last_hash: self.last_hash().await,
            event_count: self.len().await as u64,
            next_sequence: self.current_sequence().await,
            // Full-retention stores have no separate JetStream cursor and wake by
            // full replay (snapshots are disabled for them); `0` is inert.
            last_stream_seq: 0,
            // The event store has no topology; the registry's `write_snapshot`
            // fills this from `service.get_topology()` before persisting.
            topology: None,
        }
    }

    /// Seed the store's base from a hibernation snapshot, so a wake resumes from
    /// the snapshot baseline instead of replaying the full log. After seeding,
    /// the consumer replays only the post-snapshot delta
    /// (`ByStartSequence(snapshot.last_stream_seq + 1)`).
    ///
    /// Default is a no-op (full-retention stores wake by full replay; seeding
    /// would double-apply the prefix). The bounded `MemoryEventStore` overrides
    /// it to install the snapshot's marking/dedup/hash/count as its base.
    async fn seed_from_snapshot(&self, _snapshot: &crate::net_snapshot::NetSnapshot) {}

    /// Seed the *write authority* (next live `.sequence` + hash-chain tip) from a
    /// hibernation snapshot, for stores that keep a write cursor SEPARATE from
    /// the read cache.
    ///
    /// The in-memory `MemoryEventStore` derives `next_sequence`/`last_hash`
    /// directly from its own `Inner` state, so `seed_from_snapshot` already
    /// installs them and this is a no-op. But the NATS-backed store
    /// (`NatsEventStore`) keeps an authoritative `WriteState` that is normally
    /// seeded from the consumer's `applied_rx` watch channel. On a snapshot wake
    /// with an EMPTY post-snapshot delta the consumer applies nothing, never
    /// ticks `applied_rx`, and `WriteState.next_sequence` stays `0` — so the
    /// first live `append` would assign `.sequence == 0` (colliding with the
    /// pre-hibernate prefix) instead of continuing from `snapshot.next_sequence`.
    /// The wake path calls this RIGHT AFTER `seed_from_snapshot` to set the write
    /// cursor authoritatively, independent of whether the consumer ever ticks.
    ///
    /// Default is a no-op (full-retention stores wake by full replay and have no
    /// separate write cursor to seed).
    async fn seed_write_state(&self, _next_sequence: u64, _last_hash: Option<String>) {}

    /// Read the write cursor — next live `.sequence` and chain-tip hash — as one
    /// COHERENT pair (a single lock acquisition on stores that keep them under
    /// one lock). The snapshot-wake re-seed uses this so a torn read across
    /// separate `current_sequence()` + `last_hash()` calls cannot pin
    /// `next_sequence` to the pre-delta baseline while reading a post-delta tip
    /// — which would mint a colliding `.sequence` chained off a forked tip on the
    /// common "events landed while hibernated" wake. Default does the two reads
    /// (acceptable for full-retention stores, which never take the re-seed path).
    async fn write_cursor(&self) -> (u64, Option<String>) {
        (self.current_sequence().await, self.last_hash().await)
    }

    /// Seed map for the `(place_id, dedup_id)` idempotency index.
    ///
    /// Default scans `all_events()` for `TokenCreated` events carrying a
    /// non-empty `dedup_id`. A bounded store overrides this to merge the
    /// dedup entries of evicted events (folded into its base) with a scan of
    /// the resident tail, so dropped-prefix dedup keys survive.
    async fn dedup_seed(&self) -> DedupSeed {
        let mut m = DedupSeed::new();
        for e in self.all_events().await {
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
}

/// Port for topology storage (outbound).
/// Implementations provide persistence for the Petri Net structure.
pub trait TopologyRepository: Send + Sync {
    /// Get the current topology.
    fn get_topology(&self) -> Option<PetriNet>;

    /// Set/replace the topology.
    fn set_topology(&self, net: PetriNet);

    /// Clear the topology.
    fn clear(&self);

    /// Update a transition's script and guard in-place.
    /// Returns true if the transition was found and updated.
    fn update_transition_script(
        &self,
        transition_id: &TransitionId,
        script: String,
        guard: Option<String>,
    ) -> bool;
}

/// Port for state projection (outbound).
/// Implementations compute current state from events.
pub trait StateProjection: Send + Sync {
    /// Compute the current marking by replaying all events.
    fn project(&self, events: &[PersistedEvent]) -> Marking;

    /// Project starting from an existing base marking, folding only `events`
    /// on top. `apply_event_to_marking` is a pure left-fold, so
    /// `project(evs) == project_onto(&Marking::new(), evs)` and
    /// `project_onto(project(prefix), suffix) == project(prefix ++ suffix)`.
    fn project_onto(&self, base: &Marking, events: &[PersistedEvent]) -> Marking {
        let mut marking = base.clone();
        for persisted in events {
            apply_event_to_marking(&mut marking, &persisted.event);
        }
        marking
    }

    /// Apply a single event to an existing marking (incremental projection).
    ///
    /// Default implementation handles all standard event types. Override only
    /// if you need custom projection logic.
    fn apply_event(&self, marking: &mut Marking, event: &DomainEvent) {
        apply_event_to_marking(marking, event);
    }
}

// `apply_event_to_marking` is now in `petri_domain::projection` and
// re-exported via the `use` at the top of this file. Tests below
// continue to verify the behavior through the re-export.

/// Port for recording net activity (outbound), used to drive idle-based
/// hibernation. Implementations note that a net was interacted with, resetting
/// its idle timer.
///
/// This is a port so the activity signal can be raised at *every* stimulus
/// boundary — both the NATS listeners and the HTTP command handlers — without
/// the API layer depending on the concrete (NATS-KV-backed) tracker. A net's
/// liveness must not depend on which transport drove it.
#[async_trait::async_trait]
pub trait ActivitySink: Send + Sync {
    /// Record that `net_id` was just interacted with (resets its idle timer).
    /// Best-effort: implementations swallow failures.
    async fn record_activity(&self, net_id: &str);
}

#[cfg(test)]
mod tests {
    use super::*;
    use petri_domain::{PlaceId, Token, TokenColor, TransitionId};

    /// A `TokenCreated` `PersistedEvent` carrying a deterministic one-shot
    /// `dedup_id` — the shape the bounded ring indexes.
    fn one_shot(place: &PlaceId, dedup_id: &str) -> PersistedEvent {
        PersistedEvent::new(
            0,
            DomainEvent::TokenCreated {
                token: Token::new(TokenColor::Unit),
                place_id: place.clone(),
                place_name: None,
                workflow_id: None,
                signal_key: None,
                dedup_id: Some(dedup_id.to_string()),
            },
            None,
        )
    }

    /// (a) The ring caps at `K` and FIFO-evicts the OLDEST entries first.
    #[test]
    fn bounded_dedup_evicts_oldest_beyond_cap() {
        let place = PlaceId::named("p");
        let mut ring = BoundedDedup::with_cap(4);
        for i in 0..6 {
            let id = format!("id-{i}");
            ring.insert((place.clone(), id.clone()), one_shot(&place, &id));
        }
        assert_eq!(ring.len(), 4, "ring must cap at K=4");
        // Oldest two (id-0, id-1) evicted.
        assert!(ring.get(&(place.clone(), "id-0".to_string())).is_none());
        assert!(ring.get(&(place.clone(), "id-1".to_string())).is_none());
        // Newest four retained.
        for i in 2..6 {
            assert!(
                ring.contains_key(&(place.clone(), format!("id-{i}"))),
                "id-{i} (within most-recent-K window) must be retained"
            );
        }
    }

    /// (b) A recently-applied one-shot id is recognized (the redelivery-suppress
    /// path) and `get` returns its originating event while within the window.
    #[test]
    fn bounded_dedup_recognizes_recent_within_window() {
        let place = PlaceId::named("p");
        let mut ring = BoundedDedup::with_cap(4);
        let evt = one_shot(&place, "recent");
        let evt_seq = evt.sequence;
        ring.insert((place.clone(), "recent".to_string()), evt);
        // Fill the rest of the window but stay within K so "recent" survives.
        for i in 0..3 {
            let id = format!("filler-{i}");
            ring.insert((place.clone(), id.clone()), one_shot(&place, &id));
        }
        let key = (place.clone(), "recent".to_string());
        assert!(ring.contains_key(&key), "recent id must still be recognized");
        assert_eq!(
            ring.get(&key).map(|e| e.sequence),
            Some(evt_seq),
            "get must return the originating event"
        );
    }

    /// Re-inserting a present key updates the value WITHOUT advancing its FIFO
    /// position (no double-count in `order`), so it is not spuriously evicted.
    #[test]
    fn bounded_dedup_reinsert_preserves_fifo_position() {
        let place = PlaceId::named("p");
        let mut ring = BoundedDedup::with_cap(2);
        ring.insert((place.clone(), "a".to_string()), one_shot(&place, "a"));
        ring.insert((place.clone(), "b".to_string()), one_shot(&place, "b"));
        // Re-touch "a" — must NOT move it to the back of the FIFO order.
        ring.insert((place.clone(), "a".to_string()), one_shot(&place, "a"));
        // Insert "c": evicts the genuine oldest ("a"), keeping "b" and "c".
        ring.insert((place.clone(), "c".to_string()), one_shot(&place, "c"));
        assert_eq!(ring.len(), 2);
        assert!(ring.contains_key(&(place.clone(), "b".to_string())));
        assert!(ring.contains_key(&(place.clone(), "c".to_string())));
        assert!(!ring.contains_key(&(place.clone(), "a".to_string())));
    }

    #[test]
    fn test_apply_effect_failed_tokens_consumed() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();
        let place_b = PlaceId::new();

        // Add a token to place_a
        let token = Token::new(TokenColor::Unit);
        let token_id = token.id.clone();
        marking.add_token(place_a.clone(), token);
        assert_eq!(marking.token_count(&place_a), 1);

        // Apply EffectFailed with tokens_consumed=true
        let error_token = Token::new(TokenColor::Data(serde_json::json!({"error": "test"})));
        let event = DomainEvent::EffectFailed {
            transition_id: TransitionId::new(),
            transition_name: Some("t1".to_string()),
            consumed_tokens: vec![(place_a.clone(), token_id)],
            produced_tokens: vec![(place_b.clone(), error_token)],
            effect_handler_id: "handler".to_string(),
            error_message: "test error".to_string(),
            tokens_consumed: true,
            input_data: None,
            retryable: true,
        };

        apply_event_to_marking(&mut marking, &event);

        assert_eq!(
            marking.token_count(&place_a),
            0,
            "Token should be consumed from place_a"
        );
        assert_eq!(
            marking.token_count(&place_b),
            1,
            "Error token should be in place_b"
        );
    }

    #[test]
    fn test_apply_net_created_no_marking_change() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();
        let token = Token::new(TokenColor::Unit);
        marking.add_token(place_a.clone(), token);

        let event = DomainEvent::NetCreated {
            net_id: "test-net".to_string(),
            template_id: None,
            parameters: None,
            created_by: None,
            label: None,
        };

        apply_event_to_marking(&mut marking, &event);
        assert_eq!(
            marking.token_count(&place_a),
            1,
            "NetCreated should not change marking"
        );
    }

    #[test]
    fn test_apply_net_completed_no_marking_change() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();
        let token = Token::new(TokenColor::Unit);
        marking.add_token(place_a.clone(), token);

        let event = DomainEvent::NetCompleted {
            net_id: "test-net".to_string(),
            terminal_place_id: "done".to_string(),
            exit_code: Some(serde_json::json!(0)),
        };

        apply_event_to_marking(&mut marking, &event);
        assert_eq!(
            marking.token_count(&place_a),
            1,
            "NetCompleted should not change marking"
        );
    }

    #[test]
    fn test_apply_net_cancelled_no_marking_change() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();
        let token = Token::new(TokenColor::Unit);
        marking.add_token(place_a.clone(), token);

        let event = DomainEvent::NetCancelled {
            net_id: "test-net".to_string(),
            reason: Some("test".to_string()),
            cancelled_by: Some("admin".to_string()),
        };

        apply_event_to_marking(&mut marking, &event);
        assert_eq!(
            marking.token_count(&place_a),
            1,
            "NetCancelled should not change marking"
        );
    }

    #[test]
    fn test_apply_effect_failed_tokens_not_consumed() {
        let mut marking = Marking::new();
        let place_a = PlaceId::new();

        // Add a token to place_a
        let token = Token::new(TokenColor::Unit);
        let token_id = token.id.clone();
        marking.add_token(place_a.clone(), token);
        assert_eq!(marking.token_count(&place_a), 1);

        // Apply EffectFailed with tokens_consumed=false
        let event = DomainEvent::EffectFailed {
            transition_id: TransitionId::new(),
            transition_name: Some("t1".to_string()),
            consumed_tokens: vec![(place_a.clone(), token_id)],
            produced_tokens: vec![],
            effect_handler_id: "handler".to_string(),
            error_message: "test error".to_string(),
            tokens_consumed: false,
            input_data: None,
            retryable: true,
        };

        apply_event_to_marking(&mut marking, &event);

        assert_eq!(
            marking.token_count(&place_a),
            1,
            "Token should remain in place_a (not consumed)"
        );
    }
}
