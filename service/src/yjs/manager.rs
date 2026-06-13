use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;

use crate::yjs::persistence::{YjsPersistence, YjsPersistenceError};
use crate::yjs::room::YjsRoom;
use crate::yjs::DocKind;

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("persistence error: {0}")]
    Persistence(#[from] YjsPersistenceError),
    #[error("spawn_blocking failed: {0}")]
    SpawnBlocking(#[from] tokio::task::JoinError),
}

pub struct YjsManager {
    rooms: DashMap<Uuid, Arc<YjsRoom>>,
    pub persistence: YjsPersistence,
}

impl YjsManager {
    pub fn new(persistence: YjsPersistence) -> Self {
        Self {
            rooms: DashMap::new(),
            persistence,
        }
    }

    /// Get an existing room or create one by loading the document from the database.
    ///
    /// `doc_kind` is only consulted when a room is freshly created — it stamps
    /// the room (and thus every persisted update). A room already live in the
    /// `DashMap` was created with its own kind; the keyspace is opaque (template
    /// ids and page ids are both random UUIDs, no collision), so the caller's
    /// `doc_kind` for a cache hit is irrelevant.
    pub async fn get_or_create_room(
        &self,
        doc_id: Uuid,
        doc_kind: DocKind,
    ) -> Result<Arc<YjsRoom>, ManagerError> {
        // Fast path: room already exists
        if let Some(room) = self.rooms.get(&doc_id) {
            return Ok(Arc::clone(room.value()));
        }

        // Slow path: load raw data from DB (async), then build Doc in spawn_blocking
        let persistence = self.persistence.clone();
        let room = create_room_from_db(doc_id, doc_kind, persistence).await?;

        // Use entry API to handle races
        let room = self.rooms.entry(doc_id).or_insert(room).value().clone();

        tracing::info!(doc_id = %doc_id, "created Yjs room");

        Ok(room)
    }

    /// Return the live in-memory room for a doc WITHOUT creating one.
    ///
    /// `get_or_create_room` would spin up (and leak) a room as a side effect —
    /// wrong for read-only reconstruction. This is the seam the graph
    /// reconstruction uses to prefer the authoritative collaborative state
    /// (what every connected editor sees) over the persisted `yjs_documents`
    /// rows, which can lag the room across a background compaction.
    pub fn get_room_if_exists(&self, doc_id: Uuid) -> Option<Arc<YjsRoom>> {
        self.rooms.get(&doc_id).map(|room| Arc::clone(room.value()))
    }

    /// Remove a room from the manager if it has no connected clients.
    pub fn remove_room_if_empty(&self, doc_id: Uuid) {
        self.rooms.remove_if(&doc_id, |_, _| true);
        tracing::info!(doc_id = %doc_id, "evicted empty Yjs room");
    }

    /// Evict the room and kick all connected clients — for doc deletion
    /// (discard draft / delete template / delete page). Without this a
    /// collaborator's open socket keeps accepting edits whose persistence
    /// INSERTs fail on the now-deleted rows, silently losing their work.
    pub async fn close_room(&self, doc_id: Uuid) {
        if let Some((_, room)) = self.rooms.remove(&doc_id) {
            room.close().await;
            tracing::info!(doc_id = %doc_id, "closed Yjs room (doc deleted)");
        }
    }
}

/// Load raw updates from DB, build Doc + Room in spawn_blocking.
async fn create_room_from_db(
    doc_id: Uuid,
    doc_kind: DocKind,
    persistence: YjsPersistence,
) -> Result<Arc<YjsRoom>, ManagerError> {
    // Async: fetch raw bytes from postgres
    let (snapshot, updates) = persistence.load_raw_updates(doc_id).await?;

    // Sync: build Doc and extract encoded state (yrs types are !Send)
    let persistence_clone = persistence.clone();
    let room = tokio::task::spawn_blocking(move || -> Result<Arc<YjsRoom>, YjsPersistenceError> {
        let doc = YjsPersistence::build_doc_from_raw(snapshot.as_deref(), &updates)?;
        Ok(Arc::new(YjsRoom::from_doc(
            doc_id,
            doc_kind,
            &doc,
            persistence_clone,
        )))
    })
    .await??;

    Ok(room)
}
