use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;

use crate::yjs::persistence::{YjsPersistence, YjsPersistenceError};
use crate::yjs::room::YjsRoom;

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
    pub async fn get_or_create_room(
        &self,
        template_id: Uuid,
    ) -> Result<Arc<YjsRoom>, ManagerError> {
        // Fast path: room already exists
        if let Some(room) = self.rooms.get(&template_id) {
            return Ok(Arc::clone(room.value()));
        }

        // Slow path: load raw data from DB (async), then build Doc in spawn_blocking
        let persistence = self.persistence.clone();
        let room = create_room_from_db(template_id, persistence).await?;

        // Use entry API to handle races
        let room = self
            .rooms
            .entry(template_id)
            .or_insert(room)
            .value()
            .clone();

        tracing::info!(template_id = %template_id, "created Yjs room");

        Ok(room)
    }

    /// Remove a room from the manager if it has no connected clients.
    pub fn remove_room_if_empty(&self, template_id: Uuid) {
        self.rooms.remove_if(&template_id, |_, _| true);
        tracing::info!(template_id = %template_id, "evicted empty Yjs room");
    }
}

/// Load raw updates from DB, build Doc + Room in spawn_blocking.
async fn create_room_from_db(
    template_id: Uuid,
    persistence: YjsPersistence,
) -> Result<Arc<YjsRoom>, ManagerError> {
    // Async: fetch raw bytes from postgres
    let (snapshot, updates) = persistence.load_raw_updates(template_id).await?;

    // Sync: build Doc and extract encoded state (yrs types are !Send)
    let persistence_clone = persistence.clone();
    let room = tokio::task::spawn_blocking(move || -> Result<Arc<YjsRoom>, YjsPersistenceError> {
        let doc =
            YjsPersistence::build_doc_from_raw(snapshot.as_deref(), &updates)?;
        Ok(Arc::new(YjsRoom::from_doc(
            template_id,
            &doc,
            persistence_clone,
        )))
    })
    .await??;

    Ok(room)
}
