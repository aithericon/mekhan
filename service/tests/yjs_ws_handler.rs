//! Integration tests for the WebSocket handler (GET /api/yjs/{template_id}).
//!
//! Uses a real TCP server via start_test_server() + tokio-tungstenite.
//! Requires docker-compose postgres and NATS to be running.

mod common;

use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::{Doc, Map, ReadTxn, Transact, Update, WriteTxn};

/// Sync protocol message types.
const MSG_SYNC_STEP2: u8 = 1;
const MSG_SYNC_UPDATE: u8 = 2;

/// Helper: create an unpublished template via the HTTP API and return its UUID.
async fn create_template(_addr: &std::net::SocketAddr, db: &sqlx::PgPool) -> Uuid {
    // Use direct DB insert for test setup reliability
    let id = Uuid::new_v4();
    let graph = mekhan_service::models::template::WorkflowGraph::default_graph();
    let graph_json = serde_json::to_value(&graph).unwrap();

    sqlx::query(
        r#"INSERT INTO workflow_templates (id, name, description, base_template_id, version, is_latest, graph, author_id)
           VALUES ($1, 'WS Test', '', $1, 1, TRUE, $2, $3)"#,
    )
    .bind(id)
    .bind(&graph_json)
    .bind(Uuid::new_v4())
    .execute(db)
    .await
    .unwrap();

    // Seed Y.Doc for the template
    let persistence = mekhan_service::yjs::persistence::YjsPersistence::new(db.clone());
    persistence.init_doc_from_graph(id, &graph).await.unwrap();

    id
}

/// Helper: publish a template by setting published=true and compiling AIR.
async fn publish_template(db: &sqlx::PgPool, template_id: Uuid) {
    // Minimal AIR JSON for the test
    let air = json!({"places": [], "transitions": []});
    sqlx::query(
        "UPDATE workflow_templates SET published = TRUE, published_at = NOW(), air_json = $2 WHERE id = $1",
    )
    .bind(template_id)
    .bind(&air)
    .execute(db)
    .await
    .unwrap();
}

// ---------------------------------------------------------------------------
// 1. WS upgrade succeeds for unpublished template; first msg is SyncStep2
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ws_upgrade_succeeds() {
    let (addr, db) = common::start_test_server().await;
    let template_id = create_template(&addr, &db).await;

    let url = format!("ws://{addr}/api/yjs/{template_id}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // First binary message should be SyncStep2 (initial sync)
    let msg = ws.next().await.unwrap().unwrap();
    let data = msg.into_data();
    assert!(!data.is_empty(), "first message should not be empty");
    assert_eq!(data[0], MSG_SYNC_STEP2, "first message type should be SyncStep2");

    ws.close(None).await.ok();
}

// ---------------------------------------------------------------------------
// 2. WS to missing template returns HTTP error (connection refused / error)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ws_404_for_missing_template() {
    let (addr, _db) = common::start_test_server().await;
    let random_id = Uuid::new_v4();

    let url = format!("ws://{addr}/api/yjs/{random_id}");
    let result = tokio_tungstenite::connect_async(&url).await;

    // The server should reject the upgrade; tokio-tungstenite returns an error
    assert!(
        result.is_err(),
        "WS connect to missing template should fail"
    );
}

// ---------------------------------------------------------------------------
// 3. WS to a published template connects in read-only mode (server still
//    serves the initial sync so the editor can render the frozen graph)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ws_published_template_is_readonly() {
    let (addr, db) = common::start_test_server().await;
    let template_id = create_template(&addr, &db).await;
    publish_template(&db, template_id).await;

    let url = format!("ws://{addr}/api/yjs/{template_id}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("WS connect to published template should succeed (read-only)");

    // First message is the initial SyncStep2 snapshot.
    let msg = ws.next().await.unwrap().unwrap();
    let data = msg.into_data();
    assert_eq!(
        data[0], MSG_SYNC_STEP2,
        "first message should be SyncStep2 even for published templates"
    );
}

// ---------------------------------------------------------------------------
// 4. Initial sync contains graph data (Start + End nodes)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn initial_sync_contains_graph() {
    let (addr, db) = common::start_test_server().await;
    let template_id = create_template(&addr, &db).await;

    let url = format!("ws://{addr}/api/yjs/{template_id}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // Read initial SyncStep2 message
    let msg = ws.next().await.unwrap().unwrap();
    let data = msg.into_data();
    assert_eq!(data[0], MSG_SYNC_STEP2);

    let update_bytes = data[1..].to_vec();

    let has_nodes = tokio::task::spawn_blocking(move || {
        let doc = Doc::new();
        let update = Update::decode_v1(&update_bytes).unwrap();
        {
            let mut txn = doc.transact_mut();
            txn.apply_update(update).unwrap();
        }
        let txn = doc.transact();
        // New schema: nodes are in Y.Map("nodes"), keyed by nodeId → Y.Map
        if let Some(nodes_map) = txn.get_map("nodes") {
            // Default graph has Start + End nodes
            nodes_map.len(&txn) == 2
        } else {
            false
        }
    })
    .await
    .unwrap();

    assert!(has_nodes, "initial sync should contain Start + End nodes");

    ws.close(None).await.ok();
}

// ---------------------------------------------------------------------------
// 5. Two clients exchange updates
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_clients_exchange_updates() {
    let (addr, db) = common::start_test_server().await;
    let template_id = create_template(&addr, &db).await;

    let url = format!("ws://{addr}/api/yjs/{template_id}");

    // Connect client A
    let (mut ws_a, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _initial_a = ws_a.next().await.unwrap().unwrap(); // consume initial sync

    // Connect client B
    let (mut ws_b, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _initial_b = ws_b.next().await.unwrap().unwrap(); // consume initial sync

    // Client A sends a SyncUpdate
    let update = {
        let doc = Doc::new();
        {
            let mut txn = doc.transact_mut();
            let root = txn.get_or_insert_map("test_data");
            root.insert(&mut txn, "ws_test", "from_a");
        }
        let txn = doc.transact();
        txn.encode_state_as_update_v1(&yrs::StateVector::default())
    };
    let mut msg = Vec::with_capacity(1 + update.len());
    msg.push(MSG_SYNC_UPDATE);
    msg.extend_from_slice(&update);
    ws_a.send(Message::Binary(msg.into())).await.unwrap();

    // Client B should receive the broadcast
    let broadcast = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        ws_b.next(),
    )
    .await;

    assert!(broadcast.is_ok(), "client B should receive a message within timeout");
    let broadcast_msg = broadcast.unwrap().unwrap().unwrap();
    let broadcast_data = broadcast_msg.into_data();
    assert!(!broadcast_data.is_empty(), "broadcast message should not be empty");

    ws_a.close(None).await.ok();
    ws_b.close(None).await.ok();
}

// ---------------------------------------------------------------------------
// 6. Update persisted to DB after send + disconnect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn update_persisted_to_db() {
    let (addr, db) = common::start_test_server().await;
    let template_id = create_template(&addr, &db).await;

    let url = format!("ws://{addr}/api/yjs/{template_id}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _initial = ws.next().await.unwrap().unwrap(); // consume initial sync

    // Send an update
    let update = {
        let doc = Doc::new();
        {
            let mut txn = doc.transact_mut();
            let root = txn.get_or_insert_map("test");
            root.insert(&mut txn, "persist_key", "persist_value");
        }
        let txn = doc.transact();
        txn.encode_state_as_update_v1(&yrs::StateVector::default())
    };
    let mut msg = Vec::with_capacity(1 + update.len());
    msg.push(MSG_SYNC_UPDATE);
    msg.extend_from_slice(&update);
    ws.send(Message::Binary(msg.into())).await.unwrap();

    // Small delay to let the server persist
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    ws.close(None).await.ok();

    // Check that an extra row was persisted (init had 1, update adds 1 = 2 total)
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM yjs_documents WHERE template_id = $1")
            .bind(template_id)
            .fetch_one(&db)
            .await
            .unwrap();

    assert!(count >= 2, "should have at least 2 update rows (init + WS update), got {count}");
}

// ---------------------------------------------------------------------------
// 7. Disconnect evicts room; reconnect loads fresh from DB
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disconnect_evicts_room() {
    let (addr, db) = common::start_test_server().await;
    let template_id = create_template(&addr, &db).await;

    let url = format!("ws://{addr}/api/yjs/{template_id}");

    // Connect and disconnect
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _initial = ws.next().await.unwrap().unwrap();
    ws.close(None).await.ok();

    // Wait for server to process disconnect and evict the room
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Reconnect - should load fresh from DB
    let (mut ws2, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let msg = ws2.next().await.unwrap().unwrap();
    let data = msg.into_data();

    assert_eq!(data[0], MSG_SYNC_STEP2, "reconnect should receive SyncStep2");
    assert!(data.len() > 1, "reconnect should have state from DB");

    ws2.close(None).await.ok();
}
