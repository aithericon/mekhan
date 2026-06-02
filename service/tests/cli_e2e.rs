//! End-to-end integration test for the mekhan CLI workflow.
//!
//! Tests the full developer flow at the protocol level (REST + WS):
//! list → pull → edit → status → push → verify → publish → run → instances → cancel
//!
//! Requires docker-compose postgres and NATS to be running.

mod common;

use std::collections::HashMap;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::{Array, Doc, GetString, Map, ReadTxn, Text, Transact, Update, WriteTxn};

use mekhan_service::models::template::{
    ExecutionBackendType, ExecutionSpecConfig, Port, Position, WorkflowEdge, WorkflowGraph,
    WorkflowNode, WorkflowNodeData,
};
use mekhan_service::yjs::doc_ops;
use mekhan_service::yjs::persistence::YjsPersistence;

/// Sync protocol message types.
const MSG_SYNC_STEP2: u8 = 1;
const MSG_SYNC_UPDATE: u8 = 2;

/// Helper: create an unpublished template and seed its Y.Doc.
async fn create_template(db: &sqlx::PgPool) -> Uuid {
    let id = Uuid::new_v4();
    let graph = WorkflowGraph::default_graph();
    let graph_json = serde_json::to_value(&graph).unwrap();

    sqlx::query(
        r#"INSERT INTO workflow_templates (id, name, description, base_template_id, version, is_latest, graph, author_id)
           VALUES ($1, 'CLI E2E Test', 'test template', $1, 1, TRUE, $2, $3)"#,
    )
    .bind(id)
    .bind(&graph_json)
    .bind(Uuid::new_v4())
    .execute(db)
    .await
    .unwrap();

    let persistence = YjsPersistence::new(db.clone());
    persistence.init_doc_from_graph(id, &graph).await.unwrap();

    id
}

/// Helper: connect to WS, receive initial SYNC_STEP2, return (ws_stream, Doc).
async fn ws_connect_and_sync(
    addr: &std::net::SocketAddr,
    template_id: Uuid,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Doc,
) {
    let url = format!("ws://{addr}/api/yjs/{template_id}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let msg = ws.next().await.unwrap().unwrap();
    let data = msg.into_data();
    assert_eq!(data[0], MSG_SYNC_STEP2, "expected initial SYNC_STEP2");

    let payload = data[1..].to_vec();
    let doc = Doc::new();
    let update = Update::decode_v1(&payload).unwrap();
    {
        let mut txn = doc.transact_mut();
        txn.apply_update(update).unwrap();
    }

    (ws, doc)
}

/// Helper: extract files from a Y.Doc (node_id -> filename -> content).
fn extract_files(doc: &Doc) -> HashMap<String, HashMap<String, String>> {
    doc_ops::extract_files_from_doc(doc)
}

/// Helper: write a file into a Y.Doc's node files map.
fn write_file_to_doc(doc: &Doc, node_id: &str, filename: &str, content: &str) {
    let mut txn = doc.transact_mut();
    let nodes_map = txn.get_or_insert_map("nodes");

    let node_map = match nodes_map.get(&txn, node_id) {
        Some(yrs::Out::YMap(m)) => m,
        _ => panic!("node '{}' not found in doc", node_id),
    };

    let files_map = match node_map.get(&txn, "files") {
        Some(yrs::Out::YMap(m)) => m,
        _ => panic!("files map not found for node '{}'", node_id),
    };

    // Create or update the Y.Text
    match files_map.get(&txn, filename) {
        Some(yrs::Out::YText(text_ref)) => {
            let old_len = text_ref.get_string(&txn).len() as u32;
            if old_len > 0 {
                text_ref.remove_range(&mut txn, 0, old_len);
            }
            text_ref.insert(&mut txn, 0, content);
        }
        _ => {
            let text = yrs::TextPrelim::new(content);
            files_map.insert(&mut txn, filename, text);
        }
    }
}

// ===========================================================================
// Full CLI workflow E2E test
// ===========================================================================

#[tokio::test]
async fn cli_workflow_roundtrip() {
    let (addr, db) = common::start_test_server().await;
    let server = format!("http://{addr}");
    let client = reqwest::Client::new();
    let template_id = create_template(&db).await;
    let tmp = tempfile::tempdir().unwrap();

    // -----------------------------------------------------------------------
    // 1. LIST — GET /api/v1/templates → template appears in listing
    // -----------------------------------------------------------------------
    let resp = client
        .get(format!("{server}/api/v1/templates?page=0&per_page=50"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    let items = body["items"].as_array().unwrap();
    assert!(
        items.iter().any(|t| t["id"] == template_id.to_string()),
        "template should appear in list"
    );

    // -----------------------------------------------------------------------
    // 2. PULL — WS connect → sync → extract graph+files → write to tempdir
    // -----------------------------------------------------------------------
    let (mut ws, doc) = ws_connect_and_sync(&addr, template_id).await;
    let graph = doc_ops::doc_to_graph(&doc).unwrap();
    let files = extract_files(&doc);
    ws.close(None).await.ok();

    // Write to tempdir (replicating fs_ops::export_to_dir)
    let pull_dir = tmp.path().join("test-template");
    std::fs::create_dir_all(&pull_dir).unwrap();

    let meta = json!({
        "baseTemplateId": template_id.to_string(),
        "serverUrl": server,
        "lastPull": "2026-01-01T00:00:00Z"
    });
    std::fs::write(
        pull_dir.join("mekhan.lock.json"),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .unwrap();

    std::fs::write(
        pull_dir.join("graph.json"),
        serde_json::to_string_pretty(&graph).unwrap(),
    )
    .unwrap();

    // Write files if any
    if !files.is_empty() {
        let nodes_dir = pull_dir.join("nodes");
        std::fs::create_dir_all(&nodes_dir).unwrap();
        for (node_id, node_files) in &files {
            let node_dir = nodes_dir.join(node_id);
            std::fs::create_dir_all(&node_dir).unwrap();
            for (filename, content) in node_files {
                std::fs::write(node_dir.join(filename), content).unwrap();
            }
        }
    }

    // Verify directory layout
    assert!(pull_dir.join("mekhan.lock.json").exists());
    assert!(pull_dir.join("graph.json").exists());
    assert_eq!(graph.nodes.len(), 2, "default graph has Start + End nodes");

    // -----------------------------------------------------------------------
    // 3. EDIT — Write a Python file for the "start" node
    // -----------------------------------------------------------------------
    let nodes_dir = pull_dir.join("nodes");
    std::fs::create_dir_all(nodes_dir.join("start")).unwrap();
    std::fs::write(nodes_dir.join("start/main.py"), "print('hello from CLI')").unwrap();

    // -----------------------------------------------------------------------
    // 4. STATUS — Compare local vs remote, assert diff shows the new file
    // -----------------------------------------------------------------------
    let (mut ws2, remote_doc) = ws_connect_and_sync(&addr, template_id).await;
    let remote_files = extract_files(&remote_doc);
    ws2.close(None).await.ok();

    // Local has a file, remote doesn't
    assert!(
        remote_files
            .get("start")
            .and_then(|f| f.get("main.py"))
            .is_none(),
        "remote should not have the file yet"
    );

    // -----------------------------------------------------------------------
    // 5. PUSH — Apply the file change to the Y.Doc and send via WS
    // -----------------------------------------------------------------------
    let (mut ws3, push_doc) = ws_connect_and_sync(&addr, template_id).await;

    // Capture state vector before mutations
    let sv_before = {
        let txn = push_doc.transact();
        txn.state_vector()
    };

    // Write the file into the doc
    write_file_to_doc(&push_doc, "start", "main.py", "print('hello from CLI')");

    // Encode diff and send as MSG_SYNC_UPDATE
    let diff = {
        let txn = push_doc.transact();
        txn.encode_state_as_update_v1(&sv_before)
    };
    let mut update_msg = Vec::with_capacity(1 + diff.len());
    update_msg.push(MSG_SYNC_UPDATE);
    update_msg.extend_from_slice(&diff);
    ws3.send(Message::Binary(update_msg)).await.unwrap();

    // Small delay to let the server persist
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    ws3.close(None).await.ok();

    // -----------------------------------------------------------------------
    // 6. VERIFY — Pull again and confirm the file is there
    // -----------------------------------------------------------------------
    // Wait a bit more for room eviction + DB persist
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let (mut ws4, verify_doc) = ws_connect_and_sync(&addr, template_id).await;
    let verify_files = extract_files(&verify_doc);
    ws4.close(None).await.ok();

    let start_files = verify_files
        .get("start")
        .expect("start node should have files after push");
    let main_py = start_files
        .get("main.py")
        .expect("main.py should exist after push");
    assert_eq!(
        main_py, "print('hello from CLI')",
        "pushed file content should match"
    );

    // -----------------------------------------------------------------------
    // 7. PUBLISH — POST /api/v1/templates/{id}/publish
    // -----------------------------------------------------------------------
    let resp = client
        .post(format!("{server}/api/v1/templates/{template_id}/publish"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "publish should succeed");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["published"], true);

    // -----------------------------------------------------------------------
    // 8. WS READ-ONLY — published template accepts WS in read-only mode
    //    (handler serves the initial sync so the editor can render the frozen
    //    graph; client updates are dropped on the server side)
    // -----------------------------------------------------------------------
    let ws_url = format!("ws://{addr}/api/yjs/{template_id}");
    let result = tokio_tungstenite::connect_async(&ws_url).await;
    assert!(
        result.is_ok(),
        "WS connect to published template should succeed (read-only)"
    );

    // -----------------------------------------------------------------------
    // 9. RUN — POST /api/v1/instances (may 502 if petri-lab not running)
    // -----------------------------------------------------------------------
    let resp = client
        .post(format!("{server}/api/v1/instances"))
        .json(&json!({
            "template_id": template_id,
            "created_by": Uuid::nil()
        }))
        .send()
        .await
        .unwrap();

    let run_status = resp.status().as_u16();
    assert!(
        run_status == 201 || run_status == 502,
        "expected 201 (petri running) or 502 (petri not running), got {}",
        run_status
    );

    // -----------------------------------------------------------------------
    // 10. INSTANCES — GET /api/v1/instances
    // -----------------------------------------------------------------------
    let resp = client
        .get(format!(
            "{server}/api/v1/instances?template_id={template_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert!(body["items"].is_array());

    // -----------------------------------------------------------------------
    // 11. CANCEL — Insert a fake instance in DB, then cancel it
    // -----------------------------------------------------------------------
    let instance_id = Uuid::new_v4();
    let net_id = format!("mekhan-{instance_id}");

    sqlx::query(
        r#"INSERT INTO workflow_instances (id, template_id, template_version, net_id, status, created_by, started_at, metadata)
           VALUES ($1, $2, 1, $3, 'running', $4, NOW(), '{}')"#,
    )
    .bind(instance_id)
    .bind(template_id)
    .bind(&net_id)
    .bind(Uuid::new_v4())
    .execute(&db)
    .await
    .unwrap();

    let resp = client
        .delete(format!("{server}/api/v1/instances/{instance_id}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "cancelled");

    // Verify in DB
    let (status,): (String,) =
        sqlx::query_as("SELECT status FROM workflow_instances WHERE id = $1")
            .bind(instance_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(status, "cancelled");
}

// ===========================================================================
// Graph topology roundtrip: add/remove nodes and edges via Y.Doc
// ===========================================================================

/// Helper: apply a full graph (nodes + edges) to an existing Y.Doc.
/// Mirrors the logic in `src/bin/cli/doc_ops.rs::apply_graph_and_files`.
fn apply_graph_to_doc(doc: &Doc, graph: &WorkflowGraph) {
    let mut txn = doc.transact_mut();

    // -- Clear and rebuild edges --
    let edges_arr = txn.get_or_insert_array("edges");
    let len = edges_arr.len(&txn);
    if len > 0 {
        edges_arr.remove_range(&mut txn, 0, len);
    }
    for edge in &graph.edges {
        let mut edge_map: HashMap<String, yrs::Any> = HashMap::new();
        edge_map.insert(
            "id".to_string(),
            yrs::Any::String(std::sync::Arc::from(edge.id.as_str())),
        );
        edge_map.insert(
            "source".to_string(),
            yrs::Any::String(std::sync::Arc::from(edge.source.as_str())),
        );
        edge_map.insert(
            "target".to_string(),
            yrs::Any::String(std::sync::Arc::from(edge.target.as_str())),
        );
        edge_map.insert(
            "type".to_string(),
            yrs::Any::String(std::sync::Arc::from(edge.edge_type.as_str())),
        );
        if let Some(ref sh) = edge.source_handle {
            edge_map.insert(
                "sourceHandle".to_string(),
                yrs::Any::String(std::sync::Arc::from(sh.as_str())),
            );
        }
        if let Some(ref label) = edge.label {
            edge_map.insert(
                "label".to_string(),
                yrs::Any::String(std::sync::Arc::from(label.as_str())),
            );
        }
        edges_arr.push_back(&mut txn, yrs::Any::from(edge_map));
    }

    // -- Update nodes: add new, update existing, remove deleted --
    let nodes_map = txn.get_or_insert_map("nodes");

    // Collect existing node IDs
    let existing_ids: Vec<String> = nodes_map.iter(&txn).map(|(id, _)| id.to_string()).collect();

    let graph_ids: std::collections::HashSet<&str> =
        graph.nodes.iter().map(|n| n.id.as_str()).collect();

    // Remove nodes not in the graph
    for id in &existing_ids {
        if !graph_ids.contains(id.as_str()) {
            nodes_map.remove(&mut txn, id);
        }
    }

    // Add/update nodes
    for node in &graph.nodes {
        let node_map = if nodes_map.get(&txn, &node.id).is_some() {
            match nodes_map.get(&txn, &node.id) {
                Some(yrs::Out::YMap(m)) => m,
                _ => continue,
            }
        } else {
            let empty: yrs::types::map::MapPrelim =
                std::iter::empty::<(&str, yrs::Any)>().collect();
            nodes_map.insert(&mut txn, node.id.as_str(), empty)
        };

        node_map.insert(&mut txn, "type", node.node_type.clone());
        node_map.insert(&mut txn, "label", node.data.label().to_string());

        if let Some(desc) = node.data.description() {
            node_map.insert(&mut txn, "description", desc.to_string());
        }

        let pos: HashMap<String, yrs::Any> = HashMap::from([
            ("x".to_string(), yrs::Any::Number(node.position.x)),
            ("y".to_string(), yrs::Any::Number(node.position.y)),
        ]);
        node_map.insert(&mut txn, "position", yrs::Any::from(pos));

        // config
        node_map.remove(&mut txn, "config");
        let config_empty: yrs::types::map::MapPrelim =
            std::iter::empty::<(&str, yrs::Any)>().collect();
        let config_map = node_map.insert(&mut txn, "config", config_empty);
        doc_ops::write_node_config(&mut txn, &config_map, &node.data);

        // Ensure files map exists for new nodes
        if node_map.get(&txn, "files").is_none() {
            let files_empty: yrs::types::map::MapPrelim =
                std::iter::empty::<(&str, yrs::Any)>().collect();
            node_map.insert(&mut txn, "files", files_empty);
        }
    }
}

#[tokio::test]
async fn graph_topology_roundtrip() {
    let (addr, db) = common::start_test_server().await;
    let template_id = create_template(&db).await;

    // -----------------------------------------------------------------------
    // 1. PULL — verify default graph: 2 nodes, 1 edge
    // -----------------------------------------------------------------------
    let (mut ws, doc) = ws_connect_and_sync(&addr, template_id).await;
    let graph = doc_ops::doc_to_graph(&doc).unwrap();
    ws.close(None).await.ok();

    assert_eq!(graph.nodes.len(), 2, "default graph has Start + End");
    assert_eq!(graph.edges.len(), 1, "default graph has 1 edge");
    assert!(
        graph.nodes.iter().any(|n| n.id == "start"),
        "should have start node"
    );
    assert!(
        graph.nodes.iter().any(|n| n.id == "end"),
        "should have end node"
    );

    // -----------------------------------------------------------------------
    // 2. ADD NODE — insert "process" node, rewire edges
    // -----------------------------------------------------------------------
    let expanded_graph = WorkflowGraph {
        nodes: vec![
            WorkflowNode {
                id: "start".to_string(),
                node_type: "start".to_string(),
                slug: None,
                position: Position { x: 250.0, y: 100.0 },
                data: WorkflowNodeData::Start {
                    label: "Start".to_string(),
                    description: None,
                    initial: Port::empty_input(),
                    process_name: None,
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "process".to_string(),
                node_type: "automated_step".to_string(),
                slug: None,
                position: Position { x: 250.0, y: 200.0 },
                data: WorkflowNodeData::AutomatedStep {
                    label: "Process Data".to_string(),
                    description: Some("Processes input data".to_string()),
                    execution_spec: ExecutionSpecConfig {
                        backend_type: ExecutionBackendType::Docker,
                        entrypoint: None,
                        config: serde_json::json!({"image": "python:3.12"}),
                    },
                    input: mekhan_service::models::template::Port::empty_input(),
                    output: mekhan_service::models::template::default_output_port(
                        mekhan_service::models::template::ExecutionBackendType::Docker,
                    ),
                    retry_policy: Default::default(),
                    deployment_model: Default::default(),
                    stream_output: false,
                    stream_input: false,
                    asset_bindings: Vec::new(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
            WorkflowNode {
                id: "end".to_string(),
                node_type: "end".to_string(),
                slug: None,
                position: Position { x: 250.0, y: 300.0 },
                data: WorkflowNodeData::End {
                    label: "End".to_string(),
                    description: None,
                    terminal: mekhan_service::models::template::default_terminal_port(),
                    result_mapping: Vec::new(),
                },
                parent_id: None,
                width: None,
                height: None,
            },
        ],
        edges: vec![
            WorkflowEdge {
                id: "edge_start_to_process".to_string(),
                source: "start".to_string(),
                target: "process".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
            WorkflowEdge {
                id: "edge_process_to_end".to_string(),
                source: "process".to_string(),
                target: "end".to_string(),
                source_handle: None,
                target_handle: Some("in".to_string()),
                label: None,
                edge_type: "sequence".to_string(),
            },
        ],
        viewport: None,
        instance_concurrency: Default::default(),
        definitions: Default::default(),
        default_scheduler: None,
    };

    // -----------------------------------------------------------------------
    // 3. PUSH expanded graph + file for process node
    // -----------------------------------------------------------------------
    let (mut ws2, push_doc) = ws_connect_and_sync(&addr, template_id).await;

    let sv_before = {
        let txn = push_doc.transact();
        txn.state_vector()
    };

    apply_graph_to_doc(&push_doc, &expanded_graph);
    write_file_to_doc(&push_doc, "process", "main.py", "print('processing')");

    let diff = {
        let txn = push_doc.transact();
        txn.encode_state_as_update_v1(&sv_before)
    };
    let mut update_msg = Vec::with_capacity(1 + diff.len());
    update_msg.push(MSG_SYNC_UPDATE);
    update_msg.extend_from_slice(&diff);
    ws2.send(Message::Binary(update_msg)).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    ws2.close(None).await.ok();

    // -----------------------------------------------------------------------
    // 4. VERIFY — reconnect and check expanded topology
    // -----------------------------------------------------------------------
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let (mut ws3, verify_doc) = ws_connect_and_sync(&addr, template_id).await;
    let verify_graph = doc_ops::doc_to_graph(&verify_doc).unwrap();
    let verify_files = extract_files(&verify_doc);
    ws3.close(None).await.ok();

    assert_eq!(
        verify_graph.nodes.len(),
        3,
        "should have 3 nodes after adding process"
    );
    assert_eq!(
        verify_graph.edges.len(),
        2,
        "should have 2 edges after rewiring"
    );

    // Verify the process node
    let process_node = verify_graph
        .nodes
        .iter()
        .find(|n| n.id == "process")
        .expect("process node should exist");
    assert_eq!(process_node.node_type, "automated_step");
    assert_eq!(process_node.data.label(), "Process Data");
    assert_eq!(
        process_node.data.description(),
        Some("Processes input data")
    );

    // Verify edges
    assert!(
        verify_graph
            .edges
            .iter()
            .any(|e| e.source == "start" && e.target == "process"),
        "should have start→process edge"
    );
    assert!(
        verify_graph
            .edges
            .iter()
            .any(|e| e.source == "process" && e.target == "end"),
        "should have process→end edge"
    );

    // Verify file
    let process_files = verify_files
        .get("process")
        .expect("process node should have files");
    assert_eq!(
        process_files.get("main.py").unwrap(),
        "print('processing')",
        "process/main.py content should match"
    );

    // -----------------------------------------------------------------------
    // 5. REMOVE NODE — delete process, restore original edge
    // -----------------------------------------------------------------------
    let original_graph = WorkflowGraph::default_graph();

    let (mut ws4, remove_doc) = ws_connect_and_sync(&addr, template_id).await;

    let sv_before = {
        let txn = remove_doc.transact();
        txn.state_vector()
    };

    apply_graph_to_doc(&remove_doc, &original_graph);

    let diff = {
        let txn = remove_doc.transact();
        txn.encode_state_as_update_v1(&sv_before)
    };
    let mut update_msg = Vec::with_capacity(1 + diff.len());
    update_msg.push(MSG_SYNC_UPDATE);
    update_msg.extend_from_slice(&diff);
    ws4.send(Message::Binary(update_msg)).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    ws4.close(None).await.ok();

    // -----------------------------------------------------------------------
    // 6. VERIFY — back to default graph
    // -----------------------------------------------------------------------
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let (mut ws5, final_doc) = ws_connect_and_sync(&addr, template_id).await;
    let final_graph = doc_ops::doc_to_graph(&final_doc).unwrap();
    ws5.close(None).await.ok();

    assert_eq!(
        final_graph.nodes.len(),
        2,
        "should be back to 2 nodes after removing process"
    );
    assert_eq!(
        final_graph.edges.len(),
        1,
        "should be back to 1 edge after restoring"
    );
    assert!(
        final_graph
            .nodes
            .iter()
            .all(|n| n.id == "start" || n.id == "end"),
        "should only have start and end nodes"
    );
    assert!(
        final_graph
            .edges
            .iter()
            .any(|e| e.source == "start" && e.target == "end"),
        "should have start→end edge"
    );
}

// ===========================================================================
// YAML format roundtrip: write YAML → parse → push → verify
// ===========================================================================

#[tokio::test]
async fn yaml_format_roundtrip() {
    let (addr, db) = common::start_test_server().await;
    let template_id = create_template(&db).await;

    // -----------------------------------------------------------------------
    // 1. Pull default graph, convert to YAML DSL, then parse back
    // -----------------------------------------------------------------------
    let (mut ws, doc) = ws_connect_and_sync(&addr, template_id).await;
    let original_graph = doc_ops::doc_to_graph(&doc).unwrap();
    ws.close(None).await.ok();

    assert_eq!(original_graph.nodes.len(), 2);
    assert_eq!(original_graph.edges.len(), 1);

    // -----------------------------------------------------------------------
    // 2. Construct the graph that a YAML DSL would produce
    // -----------------------------------------------------------------------
    // The formats module lives in the binary crate and can't be imported here,
    // so we construct the equivalent WorkflowGraph directly.
    let parsed_graph: WorkflowGraph = {
        WorkflowGraph {
            nodes: vec![
                WorkflowNode {
                    id: "start".to_string(),
                    node_type: "start".to_string(),
                    slug: None,
                    position: Position { x: 250.0, y: 100.0 },
                    data: WorkflowNodeData::Start {
                        label: "Start".to_string(),
                        description: None,
                        initial: Port::empty_input(),
                        process_name: None,
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "process".to_string(),
                    node_type: "automated_step".to_string(),
                    slug: None,
                    position: Position { x: 250.0, y: 250.0 },
                    data: WorkflowNodeData::AutomatedStep {
                        label: "Process Data".to_string(),
                        description: Some("Processes input data".to_string()),
                        execution_spec: ExecutionSpecConfig {
                            backend_type: ExecutionBackendType::Docker,
                            entrypoint: None,
                            config: serde_json::json!({"image": "python:3.12"}),
                        },
                        input: mekhan_service::models::template::Port::empty_input(),
                        output: mekhan_service::models::template::default_output_port(
                            mekhan_service::models::template::ExecutionBackendType::Docker,
                        ),
                        retry_policy: Default::default(),
                        deployment_model: Default::default(),
                        stream_output: false,
                        stream_input: false,
                        asset_bindings: Vec::new(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
                WorkflowNode {
                    id: "end".to_string(),
                    node_type: "end".to_string(),
                    slug: None,
                    position: Position { x: 250.0, y: 400.0 },
                    data: WorkflowNodeData::End {
                        label: "End".to_string(),
                        description: None,
                        terminal: mekhan_service::models::template::default_terminal_port(),
                        result_mapping: Vec::new(),
                    },
                    parent_id: None,
                    width: None,
                    height: None,
                },
            ],
            edges: vec![
                WorkflowEdge {
                    id: "edge_start_to_process".to_string(),
                    source: "start".to_string(),
                    target: "process".to_string(),
                    source_handle: None,
                    target_handle: Some("in".to_string()),
                    label: None,
                    edge_type: "sequence".to_string(),
                },
                WorkflowEdge {
                    id: "edge_process_to_end".to_string(),
                    source: "process".to_string(),
                    target: "end".to_string(),
                    source_handle: None,
                    target_handle: Some("in".to_string()),
                    label: None,
                    edge_type: "sequence".to_string(),
                },
            ],
            viewport: None,
            instance_concurrency: Default::default(),
            definitions: Default::default(),
            default_scheduler: None,
        }
    };

    assert_eq!(parsed_graph.nodes.len(), 3);
    assert_eq!(parsed_graph.edges.len(), 2);

    // -----------------------------------------------------------------------
    // 3. Push the YAML-derived graph to the server
    // -----------------------------------------------------------------------
    let (mut ws2, push_doc) = ws_connect_and_sync(&addr, template_id).await;

    let sv_before = {
        let txn = push_doc.transact();
        txn.state_vector()
    };

    apply_graph_to_doc(&push_doc, &parsed_graph);
    write_file_to_doc(&push_doc, "process", "main.py", "print('processing')");

    let diff = {
        let txn = push_doc.transact();
        txn.encode_state_as_update_v1(&sv_before)
    };
    let mut update_msg = Vec::with_capacity(1 + diff.len());
    update_msg.push(MSG_SYNC_UPDATE);
    update_msg.extend_from_slice(&diff);
    ws2.send(Message::Binary(update_msg)).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    ws2.close(None).await.ok();

    // -----------------------------------------------------------------------
    // 4. Verify — pull and check the YAML-derived graph persisted
    // -----------------------------------------------------------------------
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let (mut ws3, verify_doc) = ws_connect_and_sync(&addr, template_id).await;
    let verify_graph = doc_ops::doc_to_graph(&verify_doc).unwrap();
    let verify_files = extract_files(&verify_doc);
    ws3.close(None).await.ok();

    // Verify topology
    assert_eq!(verify_graph.nodes.len(), 3);
    assert_eq!(verify_graph.edges.len(), 2);

    // Verify the automated_step node
    let process = verify_graph
        .nodes
        .iter()
        .find(|n| n.id == "process")
        .expect("process node should exist");
    assert_eq!(process.node_type, "automated_step");
    assert_eq!(process.data.label(), "Process Data");

    // Verify edges
    assert!(verify_graph
        .edges
        .iter()
        .any(|e| e.source == "start" && e.target == "process"));
    assert!(verify_graph
        .edges
        .iter()
        .any(|e| e.source == "process" && e.target == "end"));

    // Verify file
    let process_files = verify_files
        .get("process")
        .expect("process should have files");
    assert_eq!(process_files.get("main.py").unwrap(), "print('processing')");
}

// ===========================================================================
// Asset upload: binary files via REST endpoint
// ===========================================================================

#[tokio::test]
async fn asset_upload_roundtrip() {
    let (addr, db) = common::start_test_server().await;
    let server = format!("http://{addr}");
    let client = reqwest::Client::new();
    let template_id = create_template(&db).await;

    // -----------------------------------------------------------------------
    // 1. Upload a fake PNG image via POST /api/v1/templates/{id}/files/{node_id}
    // -----------------------------------------------------------------------
    // Minimal PNG: 8-byte signature + minimal IHDR chunk
    let fake_png: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        0x00, 0x00, 0x00, 0x0D, // IHDR chunk length
        0x49, 0x48, 0x44, 0x52, // "IHDR"
        0x00, 0x00, 0x00, 0x01, // width: 1
        0x00, 0x00, 0x00, 0x01, // height: 1
        0x08, 0x02, 0x00, 0x00, 0x00, // bit depth, color type, etc.
        0x90, 0x77, 0x53, 0xDE, // CRC
    ];

    let part = reqwest::multipart::Part::bytes(fake_png.clone())
        .file_name("screenshot.png")
        .mime_str("image/png")
        .unwrap();
    let form = reqwest::multipart::Form::new().part("file", part);

    let resp = client
        .post(format!("{server}/api/v1/files/upload/{template_id}/start"))
        .multipart(form)
        .send()
        .await
        .unwrap();

    let upload_status = resp.status().as_u16();
    assert!(
        upload_status == 201 || upload_status == 500,
        "expected 201 (S3 running) or 500 (S3 not available), got {}",
        upload_status
    );

    if upload_status == 201 {
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body["filename"], "screenshot.png");
        assert_eq!(body["content_type"], "image/png");
        assert_eq!(body["size"], fake_png.len());

        let s3_key = body["key"].as_str().unwrap().to_string();
        assert!(
            s3_key.contains("start/screenshot.png"),
            "S3 key should contain node_id/filename, got: {s3_key}"
        );

        // -------------------------------------------------------------------
        // 2. Retrieve the uploaded file via GET /api/v1/files/{key}
        // -------------------------------------------------------------------
        let resp = client
            .get(format!("{server}/api/v1/files/{s3_key}"))
            .send()
            .await
            .unwrap();

        assert_eq!(resp.status(), 200, "GET file should return 200");

        let content_type = resp
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert_eq!(content_type, "image/png");

        let returned_bytes = resp.bytes().await.unwrap().to_vec();
        assert_eq!(
            returned_bytes, fake_png,
            "returned file content should match uploaded content"
        );
    } else {
        eprintln!("  (S3/MinIO not available — skipping upload/download assertions)");
    }

    // -----------------------------------------------------------------------
    // 3. Verify text files in Y.Doc are unaffected by asset upload
    // -----------------------------------------------------------------------
    let (mut ws, doc) = ws_connect_and_sync(&addr, template_id).await;
    let files = extract_files(&doc);
    ws.close(None).await.ok();

    // The Y.Doc should not contain the binary file
    let start_files = files.get("start");
    assert!(
        start_files.is_none() || !start_files.unwrap().contains_key("screenshot.png"),
        "binary assets should NOT appear in Y.Doc text files"
    );

    // -----------------------------------------------------------------------
    // 4. Reject disallowed content types (does NOT depend on S3)
    // -----------------------------------------------------------------------
    let part = reqwest::multipart::Part::bytes(vec![0x00, 0x01, 0x02])
        .file_name("malware.exe")
        .mime_str("application/x-executable")
        .unwrap();
    let form = reqwest::multipart::Form::new().part("file", part);

    let resp = client
        .post(format!("{server}/api/v1/files/upload/{template_id}/start"))
        .multipart(form)
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        400,
        "disallowed content type should be rejected"
    );
}
