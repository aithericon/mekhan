//! Integration tests for YjsPersistence against real Postgres.
//!
//! Requires docker-compose postgres and NATS to be running.

mod common;

use mekhan_service::models::template::WorkflowGraph;
use mekhan_service::yjs::persistence::YjsPersistence;
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::{
    Doc, GetString, Map, ReadTxn, StateVector, Text, Transact, Update as YrsUpdate, WriteTxn,
};

/// Helper: create a persistence instance from an isolated test DB,
/// with a template row inserted to satisfy the FK constraint.
async fn setup() -> (YjsPersistence, Uuid) {
    let db = common::create_test_db().await;
    let persistence = YjsPersistence::new(db);
    let template_id = Uuid::new_v4();
    let graph = WorkflowGraph::default_graph();

    // Insert a template row so FK on yjs_documents is satisfied
    sqlx::query(
        r#"INSERT INTO workflow_templates (id, name, description, base_template_id, version, is_latest, graph, author_id)
           VALUES ($1, 'Persistence Test', '', $1, 1, TRUE, $2, $3)"#,
    )
    .bind(template_id)
    .bind(serde_json::to_value(&graph).unwrap())
    .bind(Uuid::new_v4())
    .execute(persistence.pool())
    .await
    .unwrap();

    (persistence, template_id)
}

// ---------------------------------------------------------------------------
// 1. init_doc_from_graph stores an update and has_doc returns true
// ---------------------------------------------------------------------------

#[tokio::test]
async fn init_doc_from_graph_stores_update() {
    let (persistence, template_id) = setup().await;
    let graph = WorkflowGraph::default_graph();

    persistence
        .init_doc_from_graph(template_id, &graph)
        .await
        .unwrap();

    assert!(persistence.has_doc(template_id).await.unwrap());

    // Verify row exists in yjs_documents
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM yjs_documents WHERE template_id = $1")
            .bind(template_id)
            .fetch_one(persistence.pool())
            .await
            .unwrap();
    assert_eq!(count, 1);

    // Verify stored bytes are non-empty
    let (data,): (Vec<u8>,) =
        sqlx::query_as("SELECT update_data FROM yjs_documents WHERE template_id = $1 LIMIT 1")
            .bind(template_id)
            .fetch_one(persistence.pool())
            .await
            .unwrap();
    assert!(!data.is_empty());
}

// ---------------------------------------------------------------------------
// 2. has_doc returns false for unknown template
// ---------------------------------------------------------------------------

#[tokio::test]
async fn has_doc_returns_false_for_unknown() {
    let (persistence, _) = setup().await;
    let random_id = Uuid::new_v4();

    assert!(!persistence.has_doc(random_id).await.unwrap());
}

// ---------------------------------------------------------------------------
// 3. load_doc round-trips graph content (new nested Y.Map schema)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn load_doc_round_trips_graph() {
    let (persistence, template_id) = setup().await;
    let graph = WorkflowGraph::default_graph();

    persistence
        .init_doc_from_graph(template_id, &graph)
        .await
        .unwrap();

    let doc = persistence.load_doc(template_id).await.unwrap();

    // Decode the graph inside spawn_blocking (yrs types are !Send)
    let (node_count, edge_count) = tokio::task::spawn_blocking(move || {
        use yrs::Array;

        let txn = doc.transact();

        // Nodes are in Y.Map("nodes"), keyed by nodeId → Y.Map
        let nodes_map = txn.get_map("nodes").expect("nodes map should exist");
        let node_count = nodes_map.len(&txn);

        // Edges are in Y.Array("edges")
        let edges_arr = txn.get_array("edges").expect("edges array should exist");
        let edge_count = edges_arr.len(&txn);

        (node_count, edge_count)
    })
    .await
    .unwrap();

    assert_eq!(node_count, 2, "default graph has Start + End nodes");
    assert_eq!(edge_count, 1, "default graph has 1 edge");
}

// ---------------------------------------------------------------------------
// 4. store_update appends incrementally with sequential seq values
// ---------------------------------------------------------------------------

#[tokio::test]
async fn store_update_appends_incrementally() {
    let (persistence, template_id) = setup().await;
    let graph = WorkflowGraph::default_graph();

    persistence
        .init_doc_from_graph(template_id, &graph)
        .await
        .unwrap();

    // Store an extra update
    let extra_update = {
        let doc = Doc::new();
        let txn = doc.transact();
        txn.encode_state_as_update_v1(&StateVector::default())
    };
    persistence
        .store_update(template_id, &extra_update)
        .await
        .unwrap();

    // Should be 2 rows with sequential seq values
    let rows: Vec<(i64,)> =
        sqlx::query_as("SELECT seq FROM yjs_documents WHERE template_id = $1 ORDER BY seq ASC")
            .bind(template_id)
            .fetch_all(persistence.pool())
            .await
            .unwrap();

    assert_eq!(rows.len(), 2);
    assert!(rows[1].0 > rows[0].0, "seq values should be increasing");
}

// ---------------------------------------------------------------------------
// 5. load_raw_updates returns correct shape
// ---------------------------------------------------------------------------

#[tokio::test]
async fn load_raw_updates_shape() {
    let (persistence, template_id) = setup().await;
    let graph = WorkflowGraph::default_graph();

    // init creates 1 update
    persistence
        .init_doc_from_graph(template_id, &graph)
        .await
        .unwrap();

    // Store 2 more updates
    let extra = {
        let doc = Doc::new();
        let txn = doc.transact();
        txn.encode_state_as_update_v1(&StateVector::default())
    };
    persistence.store_update(template_id, &extra).await.unwrap();
    persistence.store_update(template_id, &extra).await.unwrap();

    let (snapshot, updates) = persistence.load_raw_updates(template_id).await.unwrap();

    // No compaction has happened, so snapshot should be None
    assert!(snapshot.is_none());
    assert_eq!(updates.len(), 3, "init + 2 extra = 3 updates");
}

// ---------------------------------------------------------------------------
// 6. compaction merges updates into a snapshot
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compaction_merges_to_snapshot() {
    let (persistence, template_id) = setup().await;
    let graph = WorkflowGraph::default_graph();

    persistence
        .init_doc_from_graph(template_id, &graph)
        .await
        .unwrap();

    // Store 101 more updates to trigger compaction (threshold is 100, init is 1 = total 102)
    let extra = {
        let doc = Doc::new();
        let txn = doc.transact();
        txn.encode_state_as_update_v1(&StateVector::default())
    };
    for _ in 0..101 {
        persistence.store_update(template_id, &extra).await.unwrap();
    }

    // Compaction runs in a background tokio::spawn; wait for it to finish
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Snapshot should exist now
    let (snap_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM yjs_snapshots WHERE template_id = $1")
            .bind(template_id)
            .fetch_one(persistence.pool())
            .await
            .unwrap();
    assert_eq!(snap_count, 1, "snapshot should exist after compaction");

    // Update rows covered by snapshot should be deleted
    let (update_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM yjs_documents WHERE template_id = $1")
            .bind(template_id)
            .fetch_one(persistence.pool())
            .await
            .unwrap();
    assert!(
        update_count < 102,
        "compaction should have cleaned up update rows (got {update_count})"
    );
}

// ---------------------------------------------------------------------------
// 7. build_doc_from_raw produces a valid doc with "nodes" map
// ---------------------------------------------------------------------------

#[tokio::test]
async fn build_doc_from_raw_valid() {
    let (persistence, template_id) = setup().await;
    let graph = WorkflowGraph::default_graph();

    persistence
        .init_doc_from_graph(template_id, &graph)
        .await
        .unwrap();

    let (snapshot, updates) = persistence.load_raw_updates(template_id).await.unwrap();

    let has_nodes = tokio::task::spawn_blocking(move || {
        let doc = YjsPersistence::build_doc_from_raw(snapshot.as_deref(), &updates).unwrap();
        let txn = doc.transact();
        txn.get_map("nodes").is_some()
    })
    .await
    .unwrap();

    assert!(has_nodes, "built doc should have a 'nodes' map");
}

// ---------------------------------------------------------------------------
// 8. has_doc returns true after compaction (checks snapshot table)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn has_doc_true_after_compaction() {
    let (persistence, template_id) = setup().await;
    let graph = WorkflowGraph::default_graph();

    persistence
        .init_doc_from_graph(template_id, &graph)
        .await
        .unwrap();

    // Trigger compaction by exceeding threshold
    let extra = {
        let doc = Doc::new();
        let txn = doc.transact();
        txn.encode_state_as_update_v1(&StateVector::default())
    };
    for _ in 0..101 {
        persistence.store_update(template_id, &extra).await.unwrap();
    }

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Even if all update rows were deleted, has_doc should still return true via snapshot
    assert!(
        persistence.has_doc(template_id).await.unwrap(),
        "has_doc should return true after compaction"
    );
}

// ---------------------------------------------------------------------------
// 9. Y.Text content survives round-trip through store + load
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ytext_content_roundtrips_through_persistence() {
    let (persistence, template_id) = setup().await;
    let graph = WorkflowGraph::default_graph();

    // Step 1: Init the doc with the default graph (Start + End nodes with empty files maps)
    persistence
        .init_doc_from_graph(template_id, &graph)
        .await
        .unwrap();

    // Step 2: Load the doc, then simulate a client adding a file with content.
    // This mimics what the frontend does: createFile + yCollab typing.
    let file_update = tokio::task::spawn_blocking({
        let persistence = persistence.clone();
        move || {
            let doc = tokio::runtime::Handle::current()
                .block_on(persistence.load_doc(template_id))
                .unwrap();
            let mut txn = doc.transact_mut();

            // Get the Start node's map
            let nodes_map = txn.get_or_insert_map("nodes");
            let start_node = nodes_map.get(&txn, "start");
            if let Some(yrs::Out::YMap(node_map)) = start_node {
                // Get the files map
                let files = node_map.get(&txn, "files");
                if let Some(yrs::Out::YMap(files_map)) = files {
                    // Create a Y.Text with content (simulating createFile + yCollab typing)
                    let text_ref = files_map.insert(
                        &mut txn,
                        "main.py",
                        yrs::TextPrelim::new("print('hello world')"),
                    );
                    // Also append more text (simulating additional typing)
                    text_ref.push(&mut txn, "\nx = 42");
                } else {
                    panic!("files map not found on start node");
                }
            } else {
                panic!("start node not found");
            }

            // Encode the diff (update) that includes the file creation and content
            let sv = StateVector::default();
            txn.encode_state_as_update_v1(&sv)
        }
    })
    .await
    .unwrap();

    // Step 3: Store the update (simulating the WS handler persisting the client's update)
    persistence
        .store_update(template_id, &file_update)
        .await
        .unwrap();

    // Step 4: Load the doc fresh from DB (simulating a page reload)
    let content: String = tokio::task::spawn_blocking({
        let persistence = persistence.clone();
        move || {
            let doc = tokio::runtime::Handle::current()
                .block_on(persistence.load_doc(template_id))
                .unwrap();
            let txn = doc.transact();

            // Navigate to: nodes → start → files → main.py
            let nodes_map = txn.get_map("nodes").expect("nodes map should exist");
            let start_node = nodes_map.get(&txn, "start");
            if let Some(yrs::Out::YMap(node_map)) = start_node {
                let files = node_map.get(&txn, "files");
                if let Some(yrs::Out::YMap(files_map)) = files {
                    let file = files_map.get(&txn, "main.py");
                    if let Some(yrs::Out::YText(text_ref)) = file {
                        text_ref.get_string(&txn)
                    } else {
                        panic!(
                            "main.py not found or not a Text, got: {:?}",
                            files_map.get(&txn, "main.py")
                        );
                    }
                } else {
                    panic!("files map not found on start node after reload");
                }
            } else {
                panic!("start node not found after reload");
            }
        }
    })
    .await
    .unwrap();

    assert_eq!(
        content, "print('hello world')\nx = 42",
        "Y.Text content should survive persistence round-trip"
    );
}

// ---------------------------------------------------------------------------
// 10. Y.Text content roundtrips via incremental diff updates (like real WS flow)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ytext_content_roundtrips_via_diff_updates() {
    let (persistence, template_id) = setup().await;
    let graph = WorkflowGraph::default_graph();

    // Step 1: Init doc on server side
    persistence
        .init_doc_from_graph(template_id, &graph)
        .await
        .unwrap();

    // Step 2: Simulate a client connecting and getting initial state,
    // then creating a file and typing content.
    // Each operation generates a separate diff update (not full state).
    tokio::task::spawn_blocking({
        let persistence = persistence.clone();
        move || {
            // Load server state (what the client receives via SyncStep2)
            let (snapshot, updates) = tokio::runtime::Handle::current()
                .block_on(persistence.load_raw_updates(template_id))
                .unwrap();
            let server_state = {
                let doc =
                    YjsPersistence::build_doc_from_raw(snapshot.as_deref(), &updates).unwrap();
                let txn = doc.transact();
                txn.encode_state_as_update_v1(&StateVector::default())
            };

            // Create a "client" doc and apply server state
            let client_doc = Doc::new();
            {
                let update = YrsUpdate::decode_v1(&server_state).unwrap();
                let mut txn = client_doc.transact_mut();
                txn.apply_update(update).unwrap();
            }
            // Record state vector BEFORE client changes
            let sv_before = {
                let txn = client_doc.transact();
                txn.state_vector()
            };

            // Client creates a file (like frontend's createFile)
            {
                let mut txn = client_doc.transact_mut();
                let nodes_map = txn.get_or_insert_map("nodes");
                if let Some(yrs::Out::YMap(node_map)) = nodes_map.get(&txn, "start") {
                    if let Some(yrs::Out::YMap(files_map)) = node_map.get(&txn, "files") {
                        files_map.insert(&mut txn, "test.py", yrs::TextPrelim::new(""));
                    }
                }
            }

            // Encode diff update 1 (file creation)
            let diff1 = {
                let txn = client_doc.transact();
                txn.encode_state_as_update_v1(&sv_before)
            };
            // Store diff update 1
            tokio::runtime::Handle::current()
                .block_on(persistence.store_update(template_id, &diff1))
                .unwrap();

            // Client types content character by character (like yCollab)
            {
                let txn = client_doc.transact();
                let nodes_map = txn.get_map("nodes").unwrap();
                if let Some(yrs::Out::YMap(node_map)) = nodes_map.get(&txn, "start") {
                    if let Some(yrs::Out::YMap(files_map)) = node_map.get(&txn, "files") {
                        if let Some(yrs::Out::YText(text_ref)) = files_map.get(&txn, "test.py") {
                            drop(txn);
                            // Type characters one at a time
                            for (i, ch) in "hello".chars().enumerate() {
                                let sv = {
                                    let txn = client_doc.transact();
                                    txn.state_vector()
                                };
                                {
                                    let mut txn = client_doc.transact_mut();
                                    text_ref.insert(&mut txn, i as u32, &ch.to_string());
                                }
                                let diff = {
                                    let txn = client_doc.transact();
                                    txn.encode_state_as_update_v1(&sv)
                                };
                                // Store each character as a separate diff update
                                tokio::runtime::Handle::current()
                                    .block_on(persistence.store_update(template_id, &diff))
                                    .unwrap();
                            }
                        }
                    }
                }
            }
        }
    })
    .await
    .unwrap();

    // Step 3: Reload — load all updates from DB and check content
    let content: String = tokio::task::spawn_blocking({
        let persistence = persistence.clone();
        move || {
            let doc = tokio::runtime::Handle::current()
                .block_on(persistence.load_doc(template_id))
                .unwrap();
            let txn = doc.transact();

            let nodes_map = txn.get_map("nodes").expect("nodes map should exist");
            if let Some(yrs::Out::YMap(node_map)) = nodes_map.get(&txn, "start") {
                if let Some(yrs::Out::YMap(files_map)) = node_map.get(&txn, "files") {
                    if let Some(yrs::Out::YText(text_ref)) = files_map.get(&txn, "test.py") {
                        text_ref.get_string(&txn)
                    } else {
                        panic!("test.py not found after reload");
                    }
                } else {
                    panic!("files map not found after reload");
                }
            } else {
                panic!("start node not found after reload");
            }
        }
    })
    .await
    .unwrap();

    assert_eq!(
        content, "hello",
        "Y.Text content from incremental diff updates should survive persistence round-trip"
    );
}

// Tests 11-12 were temporary cross-language pipeline tests that have been removed.
// The issue they helped diagnose (Y.Text not displaying after reload) was caused by
// CollabCodeEditor not passing initial doc content to CodeMirror, not by persistence.

// (End of persistence tests)
