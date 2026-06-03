//! A minimal rosbridge v2.0 protocol client over a WebSocket.
//!
//! ## Why not `roslibrust`'s typed API?
//!
//! `roslibrust` (the documented rosbridge client) is generic over
//! [`roslibrust_common::RosMessageType`], whose `ROS_TYPE_NAME` is a
//! **compile-time `const`**. The ROS backend works with **runtime** type names
//! (`RosConfig.interface_type`) and **runtime** JSON bodies
//! (`RosConfig.fields`). A `const` can't carry a runtime string, and rosbridge
//! requires the message type on the `advertise`/`publish`/`subscribe` ops
//! (verified live: an empty `"type":""` makes turtlesim's rosbridge silently
//! drop the publish and never deliver pose messages on subscribe).
//!
//! So this module speaks the rosbridge JSON protocol **directly** over the same
//! WebSocket transport `roslibrust` itself uses (`tokio-tungstenite`). The
//! protocol is small and stable; the op shapes here mirror
//! `roslibrust_rosbridge::comm` verbatim (`{op, topic, type, msg}` for publish,
//! `{op, service, id, args}` for call_service, etc.).
//!
//! ## Lifetime
//!
//! [`RosbridgeClient::connect`] opens a connection and spawns a reader task
//! that fans inbound frames out to in-flight service calls and topic
//! subscriptions. One client serves one job; it is dropped (closing the socket)
//! when the operation finishes.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, warn};

/// Errors raised by the rosbridge client.
#[derive(Debug, thiserror::Error)]
pub enum RosbridgeError {
    /// The WebSocket connection / handshake failed.
    #[error("rosbridge connect failed: {0}")]
    Connect(String),
    /// Sending a frame on the WebSocket failed.
    #[error("rosbridge send failed: {0}")]
    Send(String),
    /// A service call returned `result: false` (rosbridge surfaced a server
    /// error), carrying the rosbridge `values` string.
    #[error("rosbridge service call failed: {0}")]
    ServiceFailed(String),
    /// The connection dropped while an operation was in flight.
    #[error("rosbridge connection closed before reply")]
    Closed,
}

/// Shared reader-task state: routing tables for in-flight service calls and
/// active topic subscriptions, keyed by rosbridge `id` / topic name.
#[derive(Default)]
struct Routes {
    /// Pending `call_service` replies, keyed by the request `id`.
    service_calls: HashMap<String, oneshot::Sender<ServiceReply>>,
    /// Active subscriptions, keyed by topic; each inbound `publish` frame is
    /// forwarded to the channel.
    subscriptions: HashMap<String, mpsc::UnboundedSender<Value>>,
}

/// A decoded `service_response` frame.
struct ServiceReply {
    result: bool,
    values: Value,
}

/// A connected rosbridge client.
///
/// Cloneable handle is not needed: one job uses one client on one task. The
/// writer half is behind a `Mutex` so the (single) caller can issue ops; the
/// reader half runs in a spawned task that drives [`Routes`].
pub struct RosbridgeClient {
    writer: Arc<Mutex<WsSink>>,
    routes: Arc<Mutex<Routes>>,
    reader: JoinHandle<()>,
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsSink = futures_util::stream::SplitSink<WsStream, Message>;

impl Drop for RosbridgeClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl RosbridgeClient {
    /// Connect to a rosbridge_server at `ws_url` (e.g. `ws://localhost:9090`)
    /// and spawn the inbound-frame reader task.
    pub async fn connect(ws_url: &str) -> Result<Self, RosbridgeError> {
        let (stream, _resp) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| RosbridgeError::Connect(e.to_string()))?;
        let (sink, mut source) = stream.split();

        let routes: Arc<Mutex<Routes>> = Arc::new(Mutex::new(Routes::default()));
        let reader_routes = routes.clone();

        let reader = tokio::spawn(async move {
            while let Some(frame) = source.next().await {
                let text = match frame {
                    Ok(Message::Text(t)) => t,
                    Ok(Message::Binary(b)) => String::from_utf8_lossy(&b).into_owned(),
                    Ok(Message::Close(_)) => break,
                    Ok(_) => continue, // ping/pong/frame — ignore
                    Err(e) => {
                        warn!(error = %e, "rosbridge read error");
                        break;
                    }
                };
                let Ok(value) = serde_json::from_str::<Value>(&text) else {
                    debug!(frame = %text, "rosbridge: non-JSON frame");
                    continue;
                };
                Self::route_frame(&reader_routes, value).await;
            }
            // On disconnect, drop all pending routes so awaiters wake with Closed.
            reader_routes.lock().await.service_calls.clear();
            reader_routes.lock().await.subscriptions.clear();
        });

        Ok(Self {
            writer: Arc::new(Mutex::new(sink)),
            routes,
            reader,
        })
    }

    /// Route one inbound rosbridge frame to the matching service call or
    /// subscription.
    async fn route_frame(routes: &Arc<Mutex<Routes>>, value: Value) {
        let op = value.get("op").and_then(Value::as_str).unwrap_or("");
        match op {
            "service_response" => {
                let id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let reply = ServiceReply {
                    result: value.get("result").and_then(Value::as_bool).unwrap_or(true),
                    values: value.get("values").cloned().unwrap_or(Value::Null),
                };
                if let Some(tx) = routes.lock().await.service_calls.remove(&id) {
                    let _ = tx.send(reply);
                }
            }
            "publish" => {
                let topic = value
                    .get("topic")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let msg = value.get("msg").cloned().unwrap_or(Value::Null);
                if let Some(tx) = routes.lock().await.subscriptions.get(&topic) {
                    let _ = tx.send(msg);
                }
            }
            other => debug!(op = other, "rosbridge: unhandled inbound op"),
        }
    }

    /// Send one JSON frame on the WebSocket.
    async fn send(&self, frame: Value) -> Result<(), RosbridgeError> {
        debug!(frame = %frame, "rosbridge send");
        self.writer
            .lock()
            .await
            .send(Message::Text(frame.to_string()))
            .await
            .map_err(|e| RosbridgeError::Send(e.to_string()))
    }

    /// Advertise + publish a single message to `topic` of ROS `type_name`.
    ///
    /// rosbridge needs the type on the `advertise` op so it can construct the
    /// message; the `publish` carries the raw `msg` body.
    pub async fn publish(
        &self,
        topic: &str,
        type_name: &str,
        msg: &Value,
    ) -> Result<(), RosbridgeError> {
        self.send(json!({
            "op": "advertise",
            "topic": topic,
            "type": type_name,
        }))
        .await?;
        self.send(json!({
            "op": "publish",
            "topic": topic,
            "type": type_name,
            "msg": msg,
        }))
        .await
    }

    /// Call service `service` with `args`, awaiting the `service_response`
    /// within `timeout`. Returns the rosbridge `values` payload.
    pub async fn call_service(
        &self,
        service: &str,
        args: &Value,
        timeout: Duration,
    ) -> Result<Value, RosbridgeError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.routes
            .lock()
            .await
            .service_calls
            .insert(id.clone(), tx);

        self.send(json!({
            "op": "call_service",
            "service": service,
            "id": id,
            "args": args,
        }))
        .await?;

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reply)) if reply.result => Ok(reply.values),
            Ok(Ok(reply)) => Err(RosbridgeError::ServiceFailed(reply.values.to_string())),
            Ok(Err(_)) => Err(RosbridgeError::Closed),
            Err(_) => {
                self.routes.lock().await.service_calls.remove(&id);
                Err(RosbridgeError::Closed)
            }
        }
    }

    /// Subscribe to `topic` of ROS `type_name` and await the FIRST message
    /// within `timeout`, then unsubscribe. Returns the message body.
    pub async fn await_first(
        &self,
        topic: &str,
        type_name: &str,
        timeout: Duration,
    ) -> Result<Value, RosbridgeError> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.routes
            .lock()
            .await
            .subscriptions
            .insert(topic.to_string(), tx);

        self.send(json!({
            "op": "subscribe",
            "topic": topic,
            "type": type_name,
        }))
        .await?;

        let result = match tokio::time::timeout(timeout, rx.recv()).await {
            Ok(Some(msg)) => Ok(msg),
            Ok(None) => Err(RosbridgeError::Closed),
            Err(_) => Err(RosbridgeError::Closed),
        };

        // Always unsubscribe + drop the route, regardless of outcome.
        self.routes.lock().await.subscriptions.remove(topic);
        let _ = self
            .send(json!({ "op": "unsubscribe", "topic": topic }))
            .await;

        result
    }
}
