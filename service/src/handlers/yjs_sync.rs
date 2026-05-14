use std::sync::atomic::{AtomicU64, Ordering};

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, Query, State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::models::template::WorkflowTemplate;
use crate::AppState;

/// Global client ID counter for uniquely identifying WebSocket connections.
static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Deserialize)]
pub struct YjsAuthQuery {
    /// Bearer token (same JWT used on HTTP routes). Browsers can't send the
    /// Authorization header on WS upgrades, so it's threaded through the query
    /// string. Validated against the same [`crate::auth::TokenVerifier`] port.
    #[serde(default)]
    pub token: Option<String>,
}

/// GET /api/yjs/{template_id} -> WebSocket upgrade
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Path(template_id): Path<Uuid>,
    Query(auth): Query<YjsAuthQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    // Pass the (possibly empty) token to the verifier and let the adapter
    // decide — matches the HTTP middleware contract so tests with the noop
    // verifier work without a token while real Zitadel deployments reject.
    let token = auth.token.as_deref().unwrap_or("");
    let claims = match state.token_verifier.verify(token).await {
        Ok(claims) => claims,
        Err(e) => {
            tracing::debug!(template_id = %template_id, "yjs ws token rejected: {e}");
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "invalid token"})),
            )
                .into_response();
        }
    };
    if let Err(e) = state.principal_resolver.resolve(claims).await {
        tracing::debug!(template_id = %template_id, "yjs ws resolver rejected: {e}");
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "invalid principal"})),
        )
            .into_response();
    }

    // Verify the template exists. Published templates connect read-only so the
    // editor can render the frozen graph; writes are dropped in `handle_socket`.
    let existing = sqlx::query_as::<_, WorkflowTemplate>(
        "SELECT * FROM workflow_templates WHERE id = $1",
    )
    .bind(template_id)
    .fetch_optional(&state.db)
    .await;

    let readonly = match existing {
        Ok(Some(t)) => t.published,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "template not found"})),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!("failed to check template for WS: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    ws.on_upgrade(move |socket| handle_socket(socket, template_id, readonly, state))
}

async fn handle_socket(socket: WebSocket, template_id: Uuid, readonly: bool, state: AppState) {
    let client_id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);

    tracing::info!(
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

    // Inbound loop: WebSocket -> room
    while let Some(result) = ws_stream.next().await {
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
        template_id = %template_id,
        client_id,
        "WebSocket disconnected"
    );
}
