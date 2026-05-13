//! Per-service `(PlaceId, dedup_id)` idempotency index.
//!
//! Prevents duplicate `TokenCreated` events when listener messages are
//! redelivered after the JetStream `duplicate_window` (120s) has expired.
//! The companion fast-path layer is `nats/src/event_store.rs::dedup_msg_id`.
//!
//! `dedup_id` is set by the publisher (slurm/nomad watchers, human result
//! listeners, bridge sender, timer firings, executor lifecycle/streaming).
//! Streaming events emit unique-per-fire ids so the index never blocks new
//! tokens; one-shot events emit deterministic ids so retries collide.
//!
//! `signal_key` (which carries lineage and is intentionally shared across
//! stream emits) is *not* used as the dedup key — that conflation caused
//! streaming metric tokens to be silently dropped.

use std::collections::HashMap;
use std::sync::Arc;

use petri_domain::{DomainEvent, PersistedEvent, PlaceId};
use tokio::sync::{OnceCell, RwLock};

use crate::EventRepository;

type Map = HashMap<(PlaceId, String), PersistedEvent>;

/// Lazy-populated map from `(place_id, dedup_id)` to the originating
/// `TokenCreated` event. First touch scans the full event log to seed,
/// rebuilding state across hibernation/wake naturally from the durable log.
#[derive(Default)]
pub(crate) struct DedupIndex {
    cell: OnceCell<Arc<RwLock<Map>>>,
}

impl DedupIndex {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn get<E: EventRepository>(
        &self,
        events: &E,
        place: &PlaceId,
        dedup_id: &str,
    ) -> Option<PersistedEvent> {
        let map = self.ensure(events).await;
        map.read()
            .await
            .get(&(place.clone(), dedup_id.to_string()))
            .cloned()
    }

    pub(crate) async fn insert<E: EventRepository>(
        &self,
        events: &E,
        place: PlaceId,
        dedup_id: String,
        event: PersistedEvent,
    ) {
        let map = self.ensure(events).await;
        map.write().await.insert((place, dedup_id), event);
    }

    async fn ensure<E: EventRepository>(&self, events: &E) -> &Arc<RwLock<Map>> {
        self.cell
            .get_or_init(|| async {
                let mut m = Map::new();
                for e in events.all_events().await {
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
                Arc::new(RwLock::new(m))
            })
            .await
    }
}
