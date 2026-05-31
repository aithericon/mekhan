//! Integration tests for YjsManager room lifecycle.
//!
//! Requires docker-compose postgres and NATS to be running.

mod common;

use std::sync::Arc;

use mekhan_service::models::template::WorkflowGraph;
use mekhan_service::yjs::manager::YjsManager;
use mekhan_service::yjs::persistence::YjsPersistence;
use uuid::Uuid;

/// Helper: create a manager with an isolated test DB.
async fn setup() -> (Arc<YjsManager>, YjsPersistence) {
    let db = common::create_test_db().await;
    let persistence = YjsPersistence::new(db);
    let manager = Arc::new(YjsManager::new(persistence.clone()));
    (manager, persistence)
}

/// Helper: create a template with a seeded Y.Doc and return its ID.
async fn seed_template(persistence: &YjsPersistence) -> Uuid {
    let template_id = Uuid::new_v4();
    let graph = WorkflowGraph::default_graph();

    // Insert a minimal template row so that foreign key constraints are satisfied
    sqlx::query(
        r#"INSERT INTO workflow_templates (id, name, description, base_template_id, version, is_latest, graph, author_id)
           VALUES ($1, 'Test', '', $1, 1, TRUE, $2, $3)"#,
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

    template_id
}

// ---------------------------------------------------------------------------
// 1. get_or_create_room creates a room on first call
// ---------------------------------------------------------------------------

#[tokio::test]
async fn creates_room_on_first_call() {
    let (manager, persistence) = setup().await;
    let template_id = seed_template(&persistence).await;

    let room = manager.get_or_create_room(template_id).await.unwrap();
    let state = room.encode_full_state().await;
    assert!(!state.is_empty(), "room should have non-empty state");
}

// ---------------------------------------------------------------------------
// 2. returns same Arc on reuse
// ---------------------------------------------------------------------------

#[tokio::test]
async fn returns_same_room_on_reuse() {
    let (manager, persistence) = setup().await;
    let template_id = seed_template(&persistence).await;

    let room1 = manager.get_or_create_room(template_id).await.unwrap();
    let room2 = manager.get_or_create_room(template_id).await.unwrap();

    assert!(
        Arc::ptr_eq(&room1, &room2),
        "same template_id should return the same Arc pointer"
    );
}

// ---------------------------------------------------------------------------
// 3. template with no Yjs data gets an empty room (empty doc)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_template_gets_empty_room() {
    let (manager, persistence) = setup().await;

    // Create a template row but do NOT seed Y.Doc
    let template_id = Uuid::new_v4();
    let graph = WorkflowGraph::default_graph();
    sqlx::query(
        r#"INSERT INTO workflow_templates (id, name, description, base_template_id, version, is_latest, graph, author_id)
           VALUES ($1, 'Empty', '', $1, 1, TRUE, $2, $3)"#,
    )
    .bind(template_id)
    .bind(serde_json::to_value(&graph).unwrap())
    .bind(Uuid::new_v4())
    .execute(persistence.pool())
    .await
    .unwrap();

    let room = manager.get_or_create_room(template_id).await.unwrap();
    // Room should exist even without pre-seeded data (empty doc)
    let state = room.encode_full_state().await;
    // The state may be a minimal yrs encoding of an empty doc, which is still non-empty bytes
    assert!(
        !state.is_empty(),
        "even empty doc should produce some encoded bytes"
    );
}

// ---------------------------------------------------------------------------
// 4. remove evicts room, re-create yields different Arc
// ---------------------------------------------------------------------------

#[tokio::test]
async fn remove_evicts_room() {
    let (manager, persistence) = setup().await;
    let template_id = seed_template(&persistence).await;

    let room1 = manager.get_or_create_room(template_id).await.unwrap();
    manager.remove_room_if_empty(template_id);

    let room2 = manager.get_or_create_room(template_id).await.unwrap();

    assert!(
        !Arc::ptr_eq(&room1, &room2),
        "after eviction, re-created room should be a different Arc"
    );
}
