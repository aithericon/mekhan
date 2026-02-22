use anyhow::{bail, Context, Result};
use futures::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

const MSG_SYNC_STEP2: u8 = 1;
const MSG_SYNC_UPDATE: u8 = 2;

/// A handle to a WebSocket-synced Y.Doc.
pub struct SyncHandle {
    ws: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    pub doc: Doc,
}

/// Connect to the Yjs WebSocket endpoint and receive the full document state.
///
/// The server sends MSG_SYNC_STEP2 (full state) immediately on connect.
pub async fn connect_and_sync(server_url: &str, template_id: &str) -> Result<SyncHandle> {
    let ws_url = server_url
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let url = format!("{}/api/yjs/{}", ws_url, template_id);

    let (mut ws, _response) = connect_async(&url)
        .await
        .context("failed to connect to WebSocket")?;

    // Receive the initial MSG_SYNC_STEP2 with full state
    let msg = ws
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("WebSocket closed before receiving state"))?
        .context("WebSocket error receiving initial state")?;

    let data = match msg {
        Message::Binary(b) => b.to_vec(),
        other => bail!("expected binary message, got: {:?}", other),
    };

    if data.is_empty() || data[0] != MSG_SYNC_STEP2 {
        bail!(
            "expected MSG_SYNC_STEP2 (0x01), got: 0x{:02x}",
            data.first().unwrap_or(&0xff)
        );
    }

    let payload = &data[1..];
    let doc = Doc::new();
    let update =
        Update::decode_v1(payload).context("failed to decode initial state update")?;
    {
        let mut txn = doc.transact_mut();
        txn.apply_update(update)
            .map_err(|e| anyhow::anyhow!("failed to apply initial state: {e}"))?;
    }

    Ok(SyncHandle { ws, doc })
}

impl SyncHandle {
    /// Capture the current state vector (call before making mutations).
    pub fn state_vector(&self) -> StateVector {
        let txn = self.doc.transact();
        txn.state_vector()
    }

    /// After local mutations: encode the diff since `old_sv` and send as MSG_SYNC_UPDATE.
    pub async fn push_update(&mut self, old_sv: &StateVector) -> Result<()> {
        let diff = {
            let txn = self.doc.transact();
            txn.encode_state_as_update_v1(old_sv)
        };

        let mut msg = Vec::with_capacity(1 + diff.len());
        msg.push(MSG_SYNC_UPDATE);
        msg.extend_from_slice(&diff);

        self.ws
            .send(Message::Binary(msg.into()))
            .await
            .context("failed to send update via WebSocket")?;

        Ok(())
    }

    /// Close the WebSocket connection.
    pub async fn disconnect(mut self) -> Result<()> {
        self.ws.close(None).await.ok();
        Ok(())
    }
}
