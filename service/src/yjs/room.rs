use std::collections::HashMap;

use tokio::sync::{mpsc, watch, RwLock};
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

use crate::yjs::persistence::{YjsPersistence, YjsPersistenceError};

/// Yjs sync protocol message types.
const MSG_SYNC_STEP1: u8 = 0;
const MSG_SYNC_STEP2: u8 = 1;
const MSG_SYNC_UPDATE: u8 = 2;

#[derive(Debug, thiserror::Error)]
pub enum RoomError {
    #[error("yrs error: {0}")]
    Yrs(String),
    #[error("persistence error: {0}")]
    Persistence(#[from] YjsPersistenceError),
    #[error("invalid message: {0}")]
    InvalidMessage(String),
    #[error("spawn_blocking failed: {0}")]
    SpawnBlocking(#[from] tokio::task::JoinError),
}

pub struct YjsRoom {
    template_id: Uuid,
    /// The full document state encoded as bytes. Updated after each mutation.
    state: RwLock<Vec<u8>>,
    clients: RwLock<HashMap<u64, mpsc::UnboundedSender<Vec<u8>>>>,
    persistence: YjsPersistence,
    /// Flipped to `true` when the backing template row is deleted out from
    /// under the room (discard draft / delete template). Connected WS handlers
    /// watch this and disconnect — otherwise their edits keep hitting the
    /// dangling `yjs_documents` FK and are silently dropped.
    closed_tx: watch::Sender<bool>,
}

impl YjsRoom {
    /// Create a room from an already-loaded Doc.
    /// Encodes the doc state immediately (must be called from sync or spawn_blocking context).
    pub fn from_doc(template_id: Uuid, doc: &Doc, persistence: YjsPersistence) -> Self {
        let txn = doc.transact();
        let state = txn.encode_state_as_update_v1(&StateVector::default());
        let (closed_tx, _) = watch::channel(false);
        Self {
            template_id,
            state: RwLock::new(state),
            clients: RwLock::new(HashMap::new()),
            persistence,
            closed_tx,
        }
    }

    /// Subscribe to the room-closed signal (see `close`).
    pub fn closed_signal(&self) -> watch::Receiver<bool> {
        self.closed_tx.subscribe()
    }

    /// Mark the room closed and kick every connected client — called when the
    /// backing template is deleted.
    pub async fn close(&self) {
        let _ = self.closed_tx.send(true);
        // Dropping the broadcast senders also unblocks each client's outbound
        // forwarder once its handler shuts down.
        self.clients.write().await.clear();
    }

    /// Add a client.
    pub async fn add_client(&self, client_id: u64, sender: mpsc::UnboundedSender<Vec<u8>>) {
        self.clients.write().await.insert(client_id, sender);
        tracing::debug!(
            template_id = %self.template_id,
            client_id,
            "client joined room"
        );
    }

    /// Remove a client. Returns the number of remaining clients.
    pub async fn remove_client(&self, client_id: u64) -> usize {
        let mut clients = self.clients.write().await;
        clients.remove(&client_id);
        let remaining = clients.len();
        tracing::debug!(
            template_id = %self.template_id,
            client_id,
            remaining,
            "client left room"
        );
        remaining
    }

    /// Get the full document state as encoded bytes (for initial sync to a new client).
    pub async fn encode_full_state(&self) -> Vec<u8> {
        self.state.read().await.clone()
    }

    /// Handle an incoming binary message from a client.
    /// Protocol:
    ///   [0, ...state_vector] -> SyncStep1: client sends its state vector, we reply with diff
    ///   [1, ...update]       -> SyncStep2: client sends missing updates (we apply + persist)
    ///   [2, ...update]       -> Update: client sends incremental update (we apply + broadcast + persist)
    pub async fn handle_message(
        &self,
        client_id: u64,
        msg: Vec<u8>,
    ) -> Result<Option<Vec<u8>>, RoomError> {
        if msg.is_empty() {
            return Err(RoomError::InvalidMessage("empty message".into()));
        }

        let msg_type = msg[0];
        let payload = msg[1..].to_vec();

        match msg_type {
            MSG_SYNC_STEP1 => {
                // Client sends its state vector; we respond with diff
                let current_state = self.state.read().await.clone();

                let diff = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, RoomError> {
                    let sv = StateVector::decode_v1(&payload)
                        .map_err(|e| RoomError::Yrs(format!("invalid state vector: {e}")))?;

                    // Reconstruct doc from our full state
                    let doc = Doc::new();
                    let full_update = Update::decode_v1(&current_state)
                        .map_err(|e| RoomError::Yrs(format!("decode state: {e}")))?;
                    {
                        let mut txn = doc.transact_mut();
                        txn.apply_update(full_update)
                            .map_err(|e| RoomError::Yrs(format!("apply state: {e}")))?;
                    }

                    let txn = doc.transact();
                    Ok(txn.encode_state_as_update_v1(&sv))
                })
                .await??;

                let mut response = Vec::with_capacity(1 + diff.len());
                response.push(MSG_SYNC_STEP2);
                response.extend_from_slice(&diff);
                Ok(Some(response))
            }

            MSG_SYNC_STEP2 => {
                // Client sends updates we're missing -- apply and persist
                let new_state = self.apply_update(&payload).await?;
                *self.state.write().await = new_state;
                self.persistence
                    .store_update(self.template_id, &payload)
                    .await?;
                Ok(None)
            }

            MSG_SYNC_UPDATE => {
                // Client sends incremental update -- apply, persist, broadcast to others
                let new_state = self.apply_update(&payload).await?;
                *self.state.write().await = new_state;
                self.persistence
                    .store_update(self.template_id, &payload)
                    .await?;
                self.broadcast(client_id, &msg).await;
                Ok(None)
            }

            _ => Err(RoomError::InvalidMessage(format!(
                "unknown message type: {msg_type}"
            ))),
        }
    }

    /// Apply an update to the current state and return the new full state.
    /// Done in spawn_blocking since yrs types are !Send.
    async fn apply_update(&self, update_data: &[u8]) -> Result<Vec<u8>, RoomError> {
        let current_state = self.state.read().await.clone();
        let update_data = update_data.to_vec();

        tokio::task::spawn_blocking(move || -> Result<Vec<u8>, RoomError> {
            let doc = Doc::new();

            // Apply current state
            if !current_state.is_empty() {
                let state_update = Update::decode_v1(&current_state)
                    .map_err(|e| RoomError::Yrs(format!("decode current state: {e}")))?;
                let mut txn = doc.transact_mut();
                txn.apply_update(state_update)
                    .map_err(|e| RoomError::Yrs(format!("apply current state: {e}")))?;
            }

            // Apply new update
            let update = Update::decode_v1(&update_data)
                .map_err(|e| RoomError::Yrs(format!("decode update: {e}")))?;
            {
                let mut txn = doc.transact_mut();
                txn.apply_update(update)
                    .map_err(|e| RoomError::Yrs(format!("apply update: {e}")))?;
            }

            // Encode new full state
            let txn = doc.transact();
            Ok(txn.encode_state_as_update_v1(&StateVector::default()))
        })
        .await?
    }

    /// Broadcast a message to all clients except the sender.
    async fn broadcast(&self, sender_id: u64, msg: &[u8]) {
        let clients = self.clients.read().await;
        for (&id, tx) in clients.iter() {
            if id != sender_id && tx.send(msg.to_vec()).is_err() {
                tracing::warn!(
                    template_id = %self.template_id,
                    client_id = id,
                    "failed to send broadcast, client likely disconnected"
                );
            }
        }
    }
}
