//! Integration tests for the PAGE Yjs WebSocket route (GET /api/yjs/page/{page_id}).
//!
//! These prove the Phase 2 gate of the Entity-Pages feature:
//!  - a page doc is its own Yjs document, keyed on `pages.id`, persisted into the
//!    GENERALIZED `yjs_documents` table with `doc_kind = 'page'`;
//!  - the round-trip survives disconnect/reconnect (lazy doc creation on first write);
//!  - the published-template read-only gate does NOT apply to pages — an Editor can
//!    write a Note attached to a *published* (frozen) template;
//!  - deleting a page reaps its yjs rows (the explicit cascade replacement that stands
//!    in for the FK `ON DELETE CASCADE` dropped when the column was renamed to doc_id).
//!
//! Uses a real TCP server via start_test_server() + tokio-tungstenite, like
//! yjs_ws_handler.rs. Requires docker-compose postgres + NATS (`just dev`).

mod common;

use futures::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;
use yrs::updates::decoder::Decode;
use yrs::{Doc, Map, ReadTxn, Transact, Update, WriteTxn};

const MSG_SYNC_UPDATE: u8 = 2;

/// Grab any seeded workspace id to satisfy the `pages.workspace_id` FK. The
/// migrations seed a default workspace; page ACL resolves via the HOST (the
/// attached template), not this column, so any valid workspace row works.
async fn any_workspace(db: &sqlx::PgPool) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as("SELECT id FROM workspaces ORDER BY created_at ASC LIMIT 1")
        .fetch_one(db)
        .await
        .expect("at least the default workspace should be seeded");
    id
}

/// Create a template (null workspace — the dev_noop principal is Editor+ on it,
/// the same setup the proven `ws_public_draft_is_writable` test relies on).
async fn create_template(db: &sqlx::PgPool, published: bool) -> Uuid {
    let id = Uuid::new_v4();
    let graph = mekhan_service::models::template::WorkflowGraph::default_graph();
    let graph_json = serde_json::to_value(&graph).unwrap();
    sqlx::query(
        r#"INSERT INTO workflow_templates (id, name, description, base_template_id, version, is_latest, graph, author_id, published, published_at)
           VALUES ($1, 'Page Host', '', $1, 1, TRUE, $2, $3, $4, CASE WHEN $4 THEN NOW() ELSE NULL END)"#,
    )
    .bind(id)
    .bind(&graph_json)
    .bind(Uuid::new_v4())
    .bind(published)
    .execute(db)
    .await
    .unwrap();
    id
}

/// Insert a `pages` row attached 1:1 to a template (the "Notes" singleton).
async fn create_page_on_template(db: &sqlx::PgPool, template_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    let ws = any_workspace(db).await;
    let actor = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO pages (id, workspace_id, title, attached_kind, attached_id, created_by, updated_by)
           VALUES ($1, $2, 'Notes', 'template', $3, $4, $4)"#,
    )
    .bind(id)
    .bind(ws)
    .bind(template_id)
    .bind(actor)
    .execute(db)
    .await
    .unwrap();
    id
}

/// Build a one-key Yjs update under Y.Map("page") and send it as a SyncUpdate.
async fn send_page_update(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    key: &str,
    value: &str,
) {
    let update = {
        let doc = Doc::new();
        {
            let mut txn = doc.transact_mut();
            let root = txn.get_or_insert_map("page");
            root.insert(&mut txn, key, value);
        }
        let txn = doc.transact();
        txn.encode_state_as_update_v1(&yrs::StateVector::default())
    };
    let mut msg = Vec::with_capacity(1 + update.len());
    msg.push(MSG_SYNC_UPDATE);
    msg.extend_from_slice(&update);
    ws.send(Message::Binary(msg)).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
}

async fn page_row_count(db: &sqlx::PgPool, page_id: Uuid) -> i64 {
    let (count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM yjs_documents WHERE doc_id = $1 AND doc_kind = 'page'")
            .bind(page_id)
            .fetch_one(db)
            .await
            .unwrap();
    count
}

// ---------------------------------------------------------------------------
// 1. WS to a missing page id is rejected at upgrade.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn page_ws_404_for_missing_page() {
    let (addr, _db) = common::start_test_server().await;
    let url = format!("ws://{addr}/api/yjs/page/{}", Uuid::new_v4());
    assert!(
        tokio_tungstenite::connect_async(&url).await.is_err(),
        "WS connect to a non-existent page must fail the upgrade"
    );
}

// ---------------------------------------------------------------------------
// 2. Round-trip: write to a page doc, reconnect, read it back. Persisted rows
//    carry doc_kind='page' in the generalized table.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn page_update_persists_with_doc_kind_page_and_survives_reconnect() {
    let (addr, db) = common::start_test_server().await;
    let template_id = create_template(&db, false).await;
    let page_id = create_page_on_template(&db, template_id).await;

    // A page starts with ZERO yjs rows (lazy creation on first write).
    assert_eq!(page_row_count(&db, page_id).await, 0, "page doc starts empty");

    let url = format!("ws://{addr}/api/yjs/page/{page_id}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _initial = ws.next().await.unwrap().unwrap(); // consume initial SyncStep2

    send_page_update(&mut ws, "title_key", "hello-pages").await;
    ws.close(None).await.ok();

    // A page-kind row landed.
    assert!(
        page_row_count(&db, page_id).await >= 1,
        "the page update must persist as a doc_kind='page' row"
    );

    // Reconnect on a fresh room (eviction happened on disconnect) → the content
    // must come back from the DB in the initial sync.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let (mut ws2, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let msg = ws2.next().await.unwrap().unwrap();
    let data = msg.into_data();
    let update_bytes = data[1..].to_vec();
    let recovered = tokio::task::spawn_blocking(move || {
        let doc = Doc::new();
        let update = Update::decode_v1(&update_bytes).unwrap();
        {
            let mut txn = doc.transact_mut();
            txn.apply_update(update).unwrap();
        }
        let txn = doc.transact();
        txn.get_map("page")
            .and_then(|m| m.get(&txn, "title_key").map(|v| v.to_string(&txn)))
    })
    .await
    .unwrap();
    assert_eq!(
        recovered.as_deref(),
        Some("hello-pages"),
        "reconnect must recover the written page content from the DB"
    );
    ws2.close(None).await.ok();
}

// ---------------------------------------------------------------------------
// 3. THE distinguishing property: a page attached to a PUBLISHED (frozen)
//    template is STILL writable by an Editor. The graph WS read-only gate must
//    NOT leak onto the page path.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn page_on_published_template_is_writable() {
    let (addr, db) = common::start_test_server().await;
    let template_id = create_template(&db, true).await; // PUBLISHED
    let page_id = create_page_on_template(&db, template_id).await;

    let url = format!("ws://{addr}/api/yjs/page/{page_id}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _initial = ws.next().await.unwrap().unwrap();

    send_page_update(&mut ws, "note", "writable-even-when-published").await;
    ws.close(None).await.ok();

    assert!(
        page_row_count(&db, page_id).await >= 1,
        "an Editor's Note on a PUBLISHED template must persist — the published \
         read-only gate must not apply to pages"
    );
}

// ---------------------------------------------------------------------------
// 4. Cascade replacement: DELETE /api/v1/pages/{id} reaps the page's yjs rows
//    (the FK ON DELETE CASCADE was dropped when template_id→doc_id).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_page_reaps_yjs_rows() {
    let (addr, db) = common::start_test_server().await;
    let template_id = create_template(&db, false).await;
    let page_id = create_page_on_template(&db, template_id).await;

    // Write so the page owns at least one yjs_documents row.
    let url = format!("ws://{addr}/api/yjs/page/{page_id}");
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let _initial = ws.next().await.unwrap().unwrap();
    send_page_update(&mut ws, "k", "v").await;
    ws.close(None).await.ok();
    assert!(page_row_count(&db, page_id).await >= 1, "precondition: page owns yjs rows");

    // dev_noop is Editor; no cookie needed.
    let resp = reqwest::Client::new()
        .delete(format!("http://{addr}/api/v1/pages/{page_id}"))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "DELETE /api/v1/pages/{{id}} should succeed for an Editor, got {}",
        resp.status()
    );

    assert_eq!(
        page_row_count(&db, page_id).await,
        0,
        "deleting the page must reap its yjs_documents rows (cascade replacement)"
    );
    let (snap,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM yjs_snapshots WHERE doc_id = $1")
        .bind(page_id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(snap, 0, "deleting the page must reap its yjs_snapshots rows too");
    let (row,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pages WHERE id = $1")
        .bind(page_id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(row, 0, "the pages row itself must be gone");
}
