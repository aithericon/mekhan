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
use crate::models::template::WorkflowTemplate;
use crate::AppState;

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

    // Tenant isolation (multi-tenancy phase 6, defense-in-depth). The Yjs room
    // is keyed by `{workspace_id}:{template_id}` so a collaborative session can
    // never bleed across workspaces even if a future ACL bug let a foreign role
    // resolve. The authoritative workspace is the *template's* own
    // `workspace_id` (a template_id belongs to exactly one workspace); we also
    // assert the caller's active workspace matches it, so a user who switched
    // active workspaces but still holds a stale grant can't reattach to another
    // tenant's room. `effective_object_role` below is the primary gate; this is
    // the belt-and-braces check. Public templates are exempt — they're read-only
    // and cross-workspace-visible by design.
    let template_workspace = template.workspace_id;
    if template.visibility != "public" {
        if let Some(user_ws) = user.workspace_id {
            if user_ws != template_workspace {
                tracing::debug!(
                    template_id = %template_id,
                    template_workspace = %template_workspace,
                    user_workspace = %user_ws,
                    "yjs ws rejected: caller's active workspace does not own this template"
                );
                return crate::models::error::ApiError::forbidden(
                    "template belongs to a different workspace",
                )
                .into_response();
            }
        }
    }

    // Object ACL: a public template connects read-only to any authenticated
    // user (publish-immutability prevents writes anyway). Otherwise the caller's
    // effective role on the template (workspace floor + folder/object grants)
    // must be ≥ Viewer to connect; a Viewer (or any sub-Editor) gets a
    // read-only socket even on an unpublished draft — `handle_socket` drops
    // their updates. This is load-bearing: a folder-scoped Editor cannot
    // collaborate unless the grant is honored here.
    let readonly = if template.visibility == "public" {
        true
    } else {
        match effective_object_role(&state.db, &user, ObjectRef::template(template.id)).await {
            Ok(Some(role)) => template.published || role < Role::Editor,
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
        handle_socket(socket, template_workspace, template_id, readonly, state)
    })
}

async fn handle_socket(
    socket: WebSocket,
    workspace_id: Uuid,
    template_id: Uuid,
    readonly: bool,
    state: AppState,
) {
    let client_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);

    // Workspace-qualified room identity (multi-tenancy phase 6). The in-memory
    // dedup + persistence key remains `template_id` (a UUID PK that already
    // belongs to exactly one workspace, so it is globally unique — no cross-
    // tenant collision is possible at the storage layer), but the logical room
    // key is `{workspace_id}:{template_id}` and is logged as such so room
    // membership is auditable per tenant.
    let room_key = format!("{workspace_id}:{template_id}");

    tracing::info!(
        room_key = %room_key,
        workspace_id = %workspace_id,
        template_id = %template_id,
        client_id,
        "WebSocket connected"
    );

    // Get or create the room
    let room = match state.yjs.get_or_create_room(template_id).await {
        Ok(room) => room,
        Err(e) => {
            tracing::error!(
                template_id = %template_id,
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
                    template_id = %template_id,
                    "room closed (template deleted); disconnecting client"
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
                    template_id = %template_id,
                    "error handling message: {e}"
                );
            }
        }
    }

    // Cleanup
    let remaining = room.remove_client(client_id).await;
    if remaining == 0 {
        state.yjs.remove_room_if_empty(template_id);
    }

    outbound_task.abort();

    tracing::info!(
        room_key = %room_key,
        template_id = %template_id,
        client_id,
        "WebSocket disconnected"
    );
}
