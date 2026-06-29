//! Net snapshot: a persisted summary of a net's in-memory state at hibernation
//! time, so a later wake resumes from the snapshot baseline instead of replaying
//! the entire durable event log.
//!
//! This module is transport-agnostic. It defines:
//! - [`NetSnapshot`] — the serializable state captured at hibernate.
//! - [`SnapshotStore`] — the outbound port a concrete store (the OpenDAL
//!   object-store adapter `ObjectSnapshotStore` in `petri-api`; see ADR-20)
//!   implements.
//!
//! ## Why a snapshot on top of the bounded tail
//!
//! The bounded event store ([`crate::EventRepository`] backed by the
//! `MemoryEventStore`) already keeps *steady-state* memory bounded — its tail
//! is byte-capped and prefix events are folded into a base marking + dedup seed.
//! But a COLD wake of a huge hibernated net still has to stream the entire NATS
//! log through the consumer (each event transiently allocated, folded, evicted):
//! peak memory is bounded, yet wake is `O(total events)` in stream reads and
//! deserialization.
//!
//! The snapshot makes wake `O(events since last hibernate)`:
//! 1. **Hibernate** captures the folded base ([`NetSnapshot`]) and the
//!    JetStream `last_stream_seq` the consumer had applied.
//! 2. **Wake** seeds the freshly-built store's base from the snapshot and starts
//!    the consumer at `DeliverPolicy::ByStartSequence(last_stream_seq + 1)`, so
//!    only the post-snapshot delta replays.
//!
//! The snapshot is a best-effort fast-path: every failure mode (no snapshot,
//! oversized, deserialize error, store unavailable) degrades cleanly to today's
//! full replay, so correctness never depends on it.

use petri_domain::{Marking, PersistedEvent, PetriNet, PlaceId};
use serde::{Deserialize, Serialize};

use crate::ports::DedupSeed;

/// One `(place_id, dedup_id)` → originating `TokenCreated` entry, serialized for
/// the snapshot. Mirrors a single entry of [`DedupSeed`] but with the place id
/// rendered to its string form so it round-trips through JSON KV.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DedupEntry {
    /// Serialized [`PlaceId`].
    pub place_id: String,
    /// The dedup key.
    pub dedup_id: String,
    /// The originating `TokenCreated` event (already hash-chained).
    pub event: PersistedEvent,
}

/// Persisted summary of a net's in-memory state at hibernation time.
///
/// Captured by [`SnapshotStore::put`] at hibernate and consumed by
/// [`SnapshotStore::get`] at wake. See the module docs for the wake protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetSnapshot {
    /// Projected marking of ALL events `[0 .. event_count)`. At wake this seeds
    /// the freshly-built store's base marking, so the marking-cache Miss path
    /// folds only the post-snapshot delta on top of it.
    pub marking: Marking,
    /// Full dedup map (base ⊕ tail at hibernate time), so the dedup window
    /// survives the wake. Restored into the store's base dedup seed.
    pub dedup: Vec<DedupEntry>,
    /// Hash of the last event folded — the chain tip the first post-wake
    /// `append` links its `previous_hash` to. `None` iff `event_count == 0`.
    pub last_hash: Option<String>,
    /// Engine storage-order count of events captured (`base_count + tail.len()`
    /// at hibernate). Seeds the store's `base_count` so post-wake `len()` keeps
    /// advancing monotonically from the right position.
    pub event_count: u64,
    /// The engine `.sequence` the next live append will use (the store's
    /// `next_sequence` at hibernate).
    pub next_sequence: u64,
    /// JetStream `stream_sequence` of the last event applied before hibernate.
    /// The wake consumer resumes at `last_stream_seq + 1`.
    pub last_stream_seq: u64,
    /// The net's topology (structure) at hibernate time. A snapshot wake replays
    /// only the post-snapshot delta (`ByStartSequence`), so the `NetInitialized`
    /// event that normally hydrates topology — it lives at the head of the log —
    /// is never replayed. Carrying the topology here makes the snapshot a
    /// self-contained wake seed (ADR-20). `None` only on pre-v2 snapshots, which
    /// the wake path treats as "cannot delta-wake → full replay".
    #[serde(default)]
    pub topology: Option<PetriNet>,
    /// Snapshot format version for forward-compat. Bump on shape changes;
    /// readers reject unknown versions (→ full replay).
    #[serde(default)]
    pub version: u32,
}

/// Current snapshot format version. v2 added `topology` (ADR-20).
pub const SNAPSHOT_VERSION: u32 = 2;

impl NetSnapshot {
    /// Rebuild a [`DedupSeed`] map from the serialized [`DedupEntry`] list.
    pub fn dedup_seed(&self) -> DedupSeed {
        let mut m = DedupSeed::new();
        for e in &self.dedup {
            m.insert(
                (PlaceId::named(e.place_id.clone()), e.dedup_id.clone()),
                e.event.clone(),
            );
        }
        m
    }

    /// Serialize a [`DedupSeed`] map into the snapshot's `dedup` entry list.
    pub fn dedup_entries(seed: &DedupSeed) -> Vec<DedupEntry> {
        seed.iter()
            .map(|((place, dedup_id), event)| DedupEntry {
                place_id: place.to_string(),
                dedup_id: dedup_id.clone(),
                event: event.clone(),
            })
            .collect()
    }
}

/// Inputs captured from the live event store at hibernate time, before they are
/// assembled into a [`NetSnapshot`].
///
/// `last_stream_seq` is now captured BY THE STORE under the same lock as the
/// marking (MAJOR 2b) — the bounded store records the JetStream `stream_sequence`
/// of each applied event in lockstep with the tail push, so the
/// `(marking, last_stream_seq)` pair this carries is always coherent. Callers no
/// longer thread a separately-read `last_stream_seq` (which could skew by an
/// event the consumer applied between the two reads while the consumer task was
/// still live).
#[derive(Debug, Clone)]
pub struct SnapshotInputs {
    pub marking: Marking,
    pub dedup: DedupSeed,
    pub last_hash: Option<String>,
    pub event_count: u64,
    pub next_sequence: u64,
    /// JetStream `stream_sequence` of the last applied event, read under the
    /// SAME store lock as `marking`. The wake resumes at `last_stream_seq + 1`.
    pub last_stream_seq: u64,
    /// The net's topology at hibernate, captured from the topology store (the
    /// event store that builds these inputs has none, so it leaves this `None`;
    /// the registry's `write_snapshot` fills it from `service.get_topology()`).
    pub topology: Option<PetriNet>,
}

impl SnapshotInputs {
    /// Assemble a [`NetSnapshot`] from these inputs. `last_stream_seq` comes from
    /// the inputs themselves (store-captured, coherent with `marking`).
    pub fn into_snapshot(self) -> NetSnapshot {
        NetSnapshot {
            marking: self.marking,
            dedup: NetSnapshot::dedup_entries(&self.dedup),
            last_hash: self.last_hash,
            event_count: self.event_count,
            next_sequence: self.next_sequence,
            last_stream_seq: self.last_stream_seq,
            topology: self.topology,
            version: SNAPSHOT_VERSION,
        }
    }
}

/// Outbound port for snapshot persistence (e.g. a NATS KV bucket).
///
/// Per-workspace, keyed by `net_id`. All operations are best-effort: a failure
/// is logged by the implementation and surfaced as `()`/`None`, never an error,
/// because every failure mode falls back to full replay.
#[async_trait::async_trait]
pub trait SnapshotStore: Send + Sync {
    /// Persist (overwrite) the snapshot for `net_id` in workspace `ws`.
    async fn put(&self, ws: &str, net_id: &str, snapshot: &NetSnapshot);

    /// Load the latest snapshot for `net_id` in workspace `ws`, or `None` if
    /// absent / unreadable / version-incompatible.
    async fn get(&self, ws: &str, net_id: &str) -> Option<NetSnapshot>;

    /// Best-effort delete of the snapshot for `net_id` (e.g. on terminal stop).
    async fn delete(&self, ws: &str, net_id: &str);
}
