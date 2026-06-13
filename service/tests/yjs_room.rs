//! Integration tests for YjsRoom message handling at the byte protocol level.
//!
//! Requires docker-compose postgres and NATS to be running.

mod common;

use std::sync::Arc;

use mekhan_service::models::template::WorkflowGraph;
use mekhan_service::yjs::persistence::YjsPersistence;
use mekhan_service::yjs::room::YjsRoom;
use tokio::sync::mpsc;
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, Map, ReadTxn, StateVector, Transact, Update, WriteTxn};

/// Protocol message type bytes.
const MSG_SYNC_STEP1: u8 = 0;
const MSG_SYNC_STEP2: u8 = 1;
const MSG_SYNC_UPDATE: u8 = 2;

/// Helper: create a room backed by a real DB with a seeded default graph.
async fn setup_room() -> (Arc<YjsRoom>, YjsPersistence, Uuid) {
    let db = common::create_test_db().await;
    let persistence = YjsPersistence::new(db);
    let template_id = Uuid::new_v4();
    let graph = WorkflowGraph::default_graph();

    // Insert a template row so FK on yjs_documents is satisfied
    sqlx::query(
        r#"INSERT INTO workflow_templates (id, name, description, base_template_id, version, is_latest, graph, author_id)
           VALUES ($1, 'Room Test', '', $1, 1, TRUE, $2, $3)"#,
    )
    .bind(template_id)
    .bind(serde_json::to_value(&graph).unwrap())
    .bind(Uuid::new_v4())
    .execute(persistence.pool())
    .await
    .unwrap();

    persistence
        .init_doc_from_graph(template_id, &graph)
        .await
        .unwrap();

    let doc = persistence.load_doc(template_id).await.unwrap();

    let persistence_clone = persistence.clone();
    let room = tokio::task::spawn_blocking(move || {
        Arc::new(YjsRoom::from_doc(template_id, mekhan_service::yjs::DocKind::Graph, &doc, persistence_clone))
    })
    .await
    .unwrap();

    (room, persistence, template_id)
}

/// Helper: create a yrs update that inserts a key into the "test_data" map.
fn make_update(key: &str, value: &str) -> Vec<u8> {
    let doc = Doc::new();
    {
        let mut txn = doc.transact_mut();
        let root = txn.get_or_insert_map("test_data");
        root.insert(&mut txn, key, value);
    }
    let txn = doc.transact();
    txn.encode_state_as_update_v1(&StateVector::default())
}

// ---------------------------------------------------------------------------
// 1. encode_full_state returns non-empty bytes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn encode_full_state_non_empty() {
    let (room, _, _) = setup_room().await;

    let state = room.encode_full_state().await;
    assert!(!state.is_empty(), "seeded room should have non-empty state");
}

// ---------------------------------------------------------------------------
// 2. add_client and remove_client returns remaining=0
// ---------------------------------------------------------------------------

#[tokio::test]
async fn add_remove_client() {
    let (room, _, _) = setup_room().await;

    let (tx, _rx) = mpsc::unbounded_channel();
    room.add_client(1, tx).await;

    let remaining = room.remove_client(1).await;
    assert_eq!(remaining, 0);
}

// ---------------------------------------------------------------------------
// 3. remove returns remaining count
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remove_returns_remaining_count() {
    let (room, _, _) = setup_room().await;

    let (tx1, _rx1) = mpsc::unbounded_channel();
    let (tx2, _rx2) = mpsc::unbounded_channel();
    let (tx3, _rx3) = mpsc::unbounded_channel();

    room.add_client(1, tx1).await;
    room.add_client(2, tx2).await;
    room.add_client(3, tx3).await;

    let remaining = room.remove_client(2).await;
    assert_eq!(remaining, 2);
}

// ---------------------------------------------------------------------------
// 4. SyncStep1 returns SyncStep2 with valid yrs data
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_step1_returns_step2() {
    let (room, _, _) = setup_room().await;

    let (tx, _rx) = mpsc::unbounded_channel();
    room.add_client(1, tx).await;

    // Build a SyncStep1 message: [0, ...state_vector]
    let sv = {
        let doc = Doc::new();
        let txn = doc.transact();
        txn.state_vector().encode_v1()
    };
    let mut msg = Vec::with_capacity(1 + sv.len());
    msg.push(MSG_SYNC_STEP1);
    msg.extend_from_slice(&sv);

    let response = room.handle_message(1, msg).await.unwrap();
    let response = response.expect("SyncStep1 should produce a response");

    assert_eq!(
        response[0], MSG_SYNC_STEP2,
        "response type should be SyncStep2"
    );
    assert!(response.len() > 1, "response payload should be non-empty");

    // Verify the payload is valid yrs update data
    let update_data = &response[1..];
    let decoded = Update::decode_v1(update_data);
    assert!(
        decoded.is_ok(),
        "response payload should be valid yrs update"
    );
}

// ---------------------------------------------------------------------------
// 5. SyncStep2 applies update and persists to DB
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_step2_applies_and_persists() {
    let (room, persistence, template_id) = setup_room().await;

    let (tx, _rx) = mpsc::unbounded_channel();
    room.add_client(1, tx).await;

    // Build a SyncStep2 message with a valid update
    let update = make_update("test_key", "test_value");
    let mut msg = Vec::with_capacity(1 + update.len());
    msg.push(MSG_SYNC_STEP2);
    msg.extend_from_slice(&update);

    let response = room.handle_message(1, msg).await.unwrap();
    assert!(response.is_none(), "SyncStep2 should return None");

    // Check that a new row was persisted
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM yjs_documents WHERE doc_id = $1")
            .bind(template_id)
            .fetch_one(persistence.pool())
            .await
            .unwrap();

    // init_doc_from_graph stored 1 row, handle_message stored another
    assert_eq!(count, 2, "should have 2 update rows (init + SyncStep2)");
}

// ---------------------------------------------------------------------------
// 6. SyncUpdate broadcasts to other clients, excludes sender
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sync_update_broadcasts_excludes_sender() {
    let (room, _, _) = setup_room().await;

    let (tx1, mut rx1) = mpsc::unbounded_channel();
    let (tx2, mut rx2) = mpsc::unbounded_channel();

    room.add_client(1, tx1).await;
    room.add_client(2, tx2).await;

    // Client 1 sends a SyncUpdate
    let update = make_update("from_client1", "hello");
    let mut msg = Vec::with_capacity(1 + update.len());
    msg.push(MSG_SYNC_UPDATE);
    msg.extend_from_slice(&update);

    room.handle_message(1, msg).await.unwrap();

    // Client 2 should receive the broadcast
    let broadcast = rx2.try_recv();
    assert!(broadcast.is_ok(), "client 2 should receive broadcast");

    // Client 1 should NOT receive the broadcast
    let self_broadcast = rx1.try_recv();
    assert!(
        self_broadcast.is_err(),
        "sender should not receive its own broadcast"
    );
}

// ---------------------------------------------------------------------------
// 7. Empty message is rejected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_message_rejected() {
    let (room, _, _) = setup_room().await;

    let (tx, _rx) = mpsc::unbounded_channel();
    room.add_client(1, tx).await;

    let result = room.handle_message(1, vec![]).await;
    assert!(result.is_err(), "empty message should be rejected");

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("empty message"),
        "error should mention empty message, got: {err_msg}"
    );
}

// ---------------------------------------------------------------------------
// 8. Unknown message type is rejected
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_type_rejected() {
    let (room, _, _) = setup_room().await;

    let (tx, _rx) = mpsc::unbounded_channel();
    room.add_client(1, tx).await;

    let result = room.handle_message(1, vec![99]).await;
    assert!(result.is_err(), "unknown type should be rejected");

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("unknown message type"),
        "error should mention unknown type, got: {err_msg}"
    );
}

// ---------------------------------------------------------------------------
// 9. Multi-client convergence: two clients send independent updates,
//    encode_full_state reflects both
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multi_client_convergence() {
    let (room, _, _) = setup_room().await;

    let (tx1, _rx1) = mpsc::unbounded_channel();
    let (tx2, _rx2) = mpsc::unbounded_channel();
    room.add_client(1, tx1).await;
    room.add_client(2, tx2).await;

    // Client 1 sends an update adding "key_a"
    let update_a = make_update("key_a", "value_a");
    let mut msg_a = Vec::with_capacity(1 + update_a.len());
    msg_a.push(MSG_SYNC_UPDATE);
    msg_a.extend_from_slice(&update_a);
    room.handle_message(1, msg_a).await.unwrap();

    // Client 2 sends an update adding "key_b"
    let update_b = make_update("key_b", "value_b");
    let mut msg_b = Vec::with_capacity(1 + update_b.len());
    msg_b.push(MSG_SYNC_UPDATE);
    msg_b.extend_from_slice(&update_b);
    room.handle_message(2, msg_b).await.unwrap();

    // Verify the full state contains both keys
    let state = room.encode_full_state().await;
    let has_both = tokio::task::spawn_blocking(move || {
        let doc = Doc::new();
        let update = Update::decode_v1(&state).unwrap();
        {
            let mut txn = doc.transact_mut();
            txn.apply_update(update).unwrap();
        }
        let txn = doc.transact();
        let root = txn
            .get_map("test_data")
            .expect("test_data map should exist");
        let has_a = root.get(&txn, "key_a").is_some();
        let has_b = root.get(&txn, "key_b").is_some();
        (has_a, has_b)
    })
    .await
    .unwrap();

    assert!(has_both.0, "state should contain key_a from client 1");
    assert!(has_both.1, "state should contain key_b from client 2");
}
