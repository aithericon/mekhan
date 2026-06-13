use std::sync::atomic::{AtomicU64, Ordering};

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, State, WebSocketUpgrade,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use axum_extra::extract::cookie::CookieJar;
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::auth::{effective_object_role, ObjectRef, Role};
use crate::models::page::Page;
use crate::models::template::WorkflowTemplate;
use crate::yjs::DocKind;
use crate::AppState;

/// Map a page to its host object reference, mirroring
/// [`crate::handlers::pages::page_host_ref`]. The `pages_placement_xor` CHECK
/// guarantees exactly one arm is reachable for a persisted row; a page's
/// effective role IS its host's. The published-template read-only gate does
/// NOT apply here — page read-only is a pure ACL floor (`role < Editor`).
fn page_object_ref(page: &Page) -> ObjectRef {
    match (page.attached_kind.as_deref(), page.attached_id, page.folder_id) {
        (Some("template"), Some(id), _) => ObjectRef::template(id),
        (Some("instance"), Some(id), _) => ObjectRef::instance(id),
        (_, _, Some(fid)) => ObjectRef::folder(fid),
        _ => unreachable!("pages_placement_xor guarantees exactly one placement arm"),
    }
}

/// Global client ID counter for uniquely identifying WebSocket connections.
static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

/// GET /api/yjs/{template_id} -> WebSocket upgrade
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(template_id): Path<Uuid>,
    headers: HeaderMap,
    jar: CookieJar,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Authenticate the upgrade via the same `mekhan_session` HttpOnly cookie
    // that gates the HTTP API. Browsers can't set an Authorization header on a
    // WS upgrade, but the same-origin cookie rides it automatically — no
    // `?token=` query param needed. Goes through the same `Authenticator` so
    // dev_noop accepts unauthenticated and bff requires a valid session.
    let user = match state.authenticator.authenticate(&headers, &jar).await {
        Ok(u) => u,
        Err(e) => {
            tracing::debug!(template_id = %template_id, "yjs ws auth rejected: {e}");
            return crate::models::error::ApiError::new(StatusCode::FORBIDDEN, "unauthenticated")
                .into_response();
        }
    };

    // Verify the template exists. Published templates connect read-only so the
    // editor can render the frozen graph; writes are dropped in `handle_socket`.
    let existing =
        sqlx::query_as::<_, WorkflowTemplate>("SELECT * FROM workflow_templates WHERE id = $1")
            .bind(template_id)
            .fetch_optional(&state.db)
            .await;

    let template = match existing {
        Ok(Some(t)) => t,
        Ok(None) => {
            return crate::models::error::ApiError::not_found("template not found").into_response();
        }
        Err(e) => {
            tracing::error!("failed to check template for WS: {e}");
            return crate::models::error::ApiError::internal(e.to_string()).into_response();
        }
    };

    // Object ACL: a public template connects read-only to any authenticated
    // user (publish-immutability prevents writes anyway). Otherwise the caller's
    // effective role on the template (workspace floor + folder/object grants)
    // must be ≥ Viewer to connect; a Viewer (or any sub-Editor) gets a
    // read-only socket even on an unpublished draft — `handle_socket` drops
    // their updates. This is load-bearing: a folder-scoped Editor cannot
    // collaborate unless the grant is honored here.
    // A *published* public template is frozen and world-viewable: any
    // authenticated user connects read-only with no workspace role required.
    // But an UNPUBLISHED public draft (e.g. a new version forked off a public
    // template — `new_version` copies `visibility`) is still being authored, so
    // it must NOT be blanket read-only: that silently dropped its owner's edits
    // (writes are discarded in `handle_socket`) so "Run draft" / publish saw a
    // stale graph. Only short-circuit on public *and published*; otherwise fall
    // through to the role check so an Editor+ on the draft can write.
    let readonly = if template.visibility == "public" && template.published {
        true
    } else {
        match effective_object_role(&state.db, &user, ObjectRef::template(template.id)).await {
            Ok(Some(role)) => template.published || role < Role::Editor,
            // No role on the object. A public draft is still world-viewable, so
            // degrade to a read-only socket (parity with public-published)
            // rather than rejecting; anything non-public stays forbidden.
            Ok(None) if template.visibility == "public" => true,
            Ok(None) => {
                tracing::debug!(
                    template_id = %template_id,
                    "yjs ws rejected: no effective role on template"
                );
                return crate::models::error::ApiError::forbidden(
                    "not authorized for this template",
                )
                .into_response();
            }
            Err(e) => {
                tracing::error!("yjs ws role resolution failed: {e}");
                return crate::models::error::ApiError::internal(e.to_string()).into_response();
            }
        }
    };

    ws.on_upgrade(move |socket| {
        handle_socket(socket, template_id, DocKind::Graph, readonly, state)
    })
}

/// GET /api/yjs/page/{page_id} -> WebSocket upgrade
///
/// The page collaboration socket. Authentication is identical to
/// [`ws_handler`] (the `mekhan_session` cookie rides the same-origin upgrade).
/// Unlike the graph handler, there is **no published-template gate**: a page's
/// read-only flag is a pure ACL floor — `host_role < Editor` connects
/// read-only, `None` is rejected. A page's host (attached template/instance, or
/// folder) is resolved via [`page_object_ref`] and its effective role gates the
/// socket.
pub async fn page_ws_handler(
    ws: WebSocketUpgrade,
    Path(page_id): Path<Uuid>,
    headers: HeaderMap,
    jar: CookieJar,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Auth identical to the graph handler.
    let user = match state.authenticator.authenticate(&headers, &jar).await {
        Ok(u) => u,
        Err(e) => {
            tracing::debug!(page_id = %page_id, "yjs page ws auth rejected: {e}");
            return crate::models::error::ApiError::new(StatusCode::FORBIDDEN, "unauthenticated")
                .into_response();
        }
    };

    // Verify the page exists.
    let existing = sqlx::query_as::<_, Page>("SELECT * FROM pages WHERE id = $1")
        .bind(page_id)
        .fetch_optional(&state.db)
        .await;

    let page = match existing {
        Ok(Some(p)) => p,
        Ok(None) => {
            return crate::models::error::ApiError::not_found("page not found").into_response();
        }
        Err(e) => {
            tracing::error!("failed to check page for WS: {e}");
            return crate::models::error::ApiError::internal(e.to_string()).into_response();
        }
    };

    // Read-only is a PURE ACL floor — NO published gate. The caller's effective
    // role on the page's HOST (template/instance/folder) must be ≥ Viewer to
    // connect; sub-Editor gets a read-only socket (writes dropped in
    // `handle_socket`); no role at all is forbidden (the upgrade is rejected).
    let readonly = match effective_object_role(&state.db, &user, page_object_ref(&page)).await {
        Ok(Some(role)) => role < Role::Editor,
        Ok(None) => {
            tracing::debug!(
                page_id = %page_id,
                "yjs page ws rejected: no effective role on host"
            );
            return crate::models::error::ApiError::forbidden("not authorized for this page")
                .into_response();
        }
        Err(e) => {
            tracing::error!("yjs page ws role resolution failed: {e}");
            return crate::models::error::ApiError::internal(e.to_string()).into_response();
        }
    };

    ws.on_upgrade(move |socket| handle_socket(socket, page_id, DocKind::Page, readonly, state))
}

async fn handle_socket(
    socket: WebSocket,
    doc_id: Uuid,
    doc_kind: DocKind,
    readonly: bool,
    state: AppState,
) {
    let client_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);

    tracing::info!(
        doc_id = %doc_id,
        ?doc_kind,
        client_id,
        "WebSocket connected"
    );

    // Get or create the room. `doc_kind` (Graph for templates, Page for pages)
    // stamps any freshly-created room and thus its persisted updates. The
    // protocol below is kind-agnostic; only the DB write seam consults the kind.
    let room = match state.yjs.get_or_create_room(doc_id, doc_kind).await {
        Ok(room) => room,
        Err(e) => {
            tracing::error!(
                doc_id = %doc_id,
                "failed to get/create room: {e}"
            );
            return;
        }
    };

    // Channel for outbound messages to this client (broadcasts from other clients)
    let (broadcast_tx, mut broadcast_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Channel for direct responses (SyncStep2 replies to this client's SyncStep1)
    let (response_tx, mut response_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Register client in the room for broadcasts
    room.add_client(client_id, broadcast_tx).await;

    let (mut ws_sink, mut ws_stream) = socket.split();

    // Send initial sync: full document state as SyncStep2
    let full_state = room.encode_full_state().await;
    let mut initial_msg = Vec::with_capacity(1 + full_state.len());
    initial_msg.push(1); // MSG_SYNC_STEP2
    initial_msg.extend_from_slice(&full_state);
    if ws_sink
        .send(Message::Binary(initial_msg.into()))
        .await
        .is_err()
    {
        tracing::warn!(client_id, "failed to send initial sync");
        room.remove_client(client_id).await;
        return;
    }

    // Spawn outbound forwarder: merges broadcasts and direct responses -> WebSocket
    let outbound_task = tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                Some(msg) = broadcast_rx.recv() => msg,
                Some(msg) = response_rx.recv() => msg,
                else => break,
            };
            if ws_sink.send(Message::Binary(msg.into())).await.is_err() {
                break;
            }
        }
    });

    // Room-closed signal: the template can be deleted while we're connected
    // (discard draft / delete template) — keep the subscription so the loop
    // below can kick this client instead of letting it edit a doc whose
    // persistence is gone.
    let mut closed = room.closed_signal();

    // Inbound loop: WebSocket -> room (or room-closed -> disconnect)
    loop {
        let result = tokio::select! {
            next = ws_stream.next() => match next {
                Some(r) => r,
                None => break,
            },
            _ = closed.changed() => {
                tracing::info!(
                    client_id,
                    doc_id = %doc_id,
                    "room closed (doc deleted); disconnecting client"
                );
                break;
            }
        };
        let msg = match result {
            Ok(Message::Binary(data)) => data.to_vec(),
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(_) | Message::Pong(_)) => continue,
            Ok(_) => continue,
            Err(e) => {
                tracing::debug!(client_id, "ws read error: {e}");
                break;
            }
        };

        // Published templates: only the initial sync (SyncStep1) is honored.
        // Updates from any client must not mutate the frozen Y.Doc.
        if readonly && msg.first().copied().is_some_and(|t| t != 0) {
            continue;
        }

        match room.handle_message(client_id, msg).await {
            Ok(Some(response)) => {
                // Direct response (e.g., SyncStep2 reply to this client's SyncStep1)
                if response_tx.send(response).is_err() {
                    break;
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    client_id,
                    doc_id = %doc_id,
                    "error handling message: {e}"
                );
            }
        }
    }

    // Cleanup
    let remaining = room.remove_client(client_id).await;
    if remaining == 0 {
        state.yjs.remove_room_if_empty(doc_id);
    }

    outbound_task.abort();

    tracing::info!(
        doc_id = %doc_id,
        client_id,
        "WebSocket disconnected"
    );
}
