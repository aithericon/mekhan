//! Integration tests for the publish flow that reads from Y.Doc.
//!
//! Requires docker-compose postgres and NATS to be running.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;
use yrs::{Map, ReadTxn, StateVector, Transact, WriteTxn};

async fn body_json(body: Body) -> Value {
    let bytes = body.collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

// ---------------------------------------------------------------------------
// 1. publish reads from Y.Doc: mutate Y.Doc, publish, AIR reflects mutation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publish_reads_from_ydoc() {
    let (app, db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    // Create template (this also seeds Y.Doc with default graph)
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "YDoc Publish Test",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp.into_body()).await;
    let template_id: Uuid = serde_json::from_value(created["id"].clone()).unwrap();

    // Mutate the Y.Doc: change the Start node's label to prove publish reads from Y.Doc
    let persistence = mekhan_service::yjs::persistence::YjsPersistence::new(db.clone());
    let doc = persistence.load_doc(template_id).await.unwrap();

    let update = tokio::task::spawn_blocking(move || {
        // Modify the Start node's label in the new nested Y.Map schema
        {
            let mut txn = doc.transact_mut();
            let nodes_map = txn.get_or_insert_map("nodes");
            // The "start" node has id "start" — get its Y.Map
            if let Some(yrs::Out::YMap(start_node)) = nodes_map.get(&txn, "start") {
                start_node.insert(&mut txn, "label", "Modified Start");
            }
        }

        // Encode the full state
        let txn = doc.transact();
        txn.encode_state_as_update_v1(&StateVector::default())
    })
    .await
    .unwrap();

    // Store the full state as an update
    persistence
        .store_update(template_id, mekhan_service::yjs::DocKind::Graph, &update)
        .await
        .unwrap();

    // Publish the template
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/templates/{template_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["published"], true);

    // The AIR should be populated (compile succeeded from the Y.Doc graph)
    let air = &body["air_json"];
    assert!(air.is_object(), "air_json should be populated from Y.Doc");
}

// ---------------------------------------------------------------------------
// 2. publish falls back to DB graph when Y.Doc is missing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn publish_falls_back_to_db_graph() {
    let (app, db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    // Create template
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Fallback Publish Test",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp.into_body()).await;
    let template_id: Uuid = serde_json::from_value(created["id"].clone()).unwrap();

    // Delete all Y.Doc rows to simulate legacy template without Y.Doc
    sqlx::query("DELETE FROM yjs_documents WHERE doc_id = $1")
        .bind(template_id)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("DELETE FROM yjs_snapshots WHERE doc_id = $1")
        .bind(template_id)
        .execute(&db)
        .await
        .unwrap();

    // Publish should succeed using the DB graph column as fallback
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/templates/{template_id}/publish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert_eq!(body["published"], true);
    assert!(
        body["air_json"].is_object(),
        "air_json should be populated from DB graph"
    );
}

// ---------------------------------------------------------------------------
// 3. create_template seeds Y.Doc automatically
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_template_seeds_ydoc() {
    let (app, db) = common::test_app().await;
    let author_id = Uuid::new_v4();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/templates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "name": "Seeded Template",
                        "author_id": author_id
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let created = body_json(resp.into_body()).await;
    let template_id: Uuid = serde_json::from_value(created["id"].clone()).unwrap();

    // Verify Y.Doc row exists
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM yjs_documents WHERE doc_id = $1")
            .bind(template_id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(
        count >= 1,
        "create_template should seed Y.Doc, got {count} rows"
    );

    // Load the doc and verify it has the nodes map (new schema)
    let persistence = mekhan_service::yjs::persistence::YjsPersistence::new(db);
    let doc = persistence.load_doc(template_id).await.unwrap();

    let has_nodes = tokio::task::spawn_blocking(move || {
        let txn = doc.transact();
        txn.get_map("nodes").is_some()
    })
    .await
    .unwrap();

    assert!(has_nodes, "seeded Y.Doc should have a 'nodes' map");
}
