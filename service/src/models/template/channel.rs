//! Streaming [`Channel`] declarations (docs/25): direction/plane/element
//! type, consumer-side join discipline and data-plane transports.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Which way a [`Channel`] flows relative to its owning node: `In` consumes
/// tokens the node reads, `Out` produces tokens the node emits.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelDirection {
    In,
    Out,
}

/// Which net plane a [`Channel`] rides on: `Control` carries slim control
/// tokens that drive net firing (the borrow resolver can reference their
/// payload fields downstream); `Data` carries out-of-band element payloads
/// (large/binary), edge-wired only and never value-referenceable.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelPlane {
    Data,
    Control,
}

/// The element type a [`Channel`] carries. `Json` declares a schema the
/// compiler typechecks against the `SchemaRegistry`; `Binary` carries opaque
/// bytes tagged by `content_type`; `Any` is an untyped passthrough.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ElementType {
    Json { schema: serde_json::Value },
    Binary { content_type: String },
    Any,
}

/// How a CONSUMER edge folds a CONTROL channel's bracketed episode (the
/// producer emits one uniform `open | item* | close` stream; the consumer's
/// `join` decides the fold). `Each` fires downstream once per `item`
/// (the old `signal` behaviour, generalised); `Gather` is the counted
/// barrier (the old `scatter` path) that collects all items, sorts by
/// `__map_idx`, and projects a single array — sized by the episode's own
/// `close.count`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ChannelJoin {
    #[default]
    Each,
    Gather,
}

/// Which out-of-band transport a DATA channel's bytes ride (docs/25 §6). This
/// is the single source of truth the producer SDK stamps into the `open`
/// descriptor and both executors dispatch on: the producer's executor picks the
/// publish adapter, the consumer's executor picks the subscribe adapter off the
/// descriptor it lifted. Ignored for `Control` channels (their payloads ride the
/// net, not a transport).
///
/// * `Jetstream` — the v1 default: reliable, ordered, replayable JetStream
///   stream with per-element ack backpressure. The tappable durable log.
/// * `NatsLatest` — lossy-latest core NATS: no ordering, no ack, no replay; a
///   late/slow consumer misses early elements (live frames / drop-stale). The
///   semantic opposite of JetStream — what proves the dispatch seam is real.
/// * `S3` — durable object store (S3 / GCS / Azure / local-fs via OpenDAL): each
///   element is one object, the consumer polls keys in order. Lossless, ordered,
///   and fully **replayable** from element zero long after the producer finished
///   — the right transport for large/durable blobs (checkpoints, datasets,
///   archived media). A different transport SHAPE (key/value, not pub/sub),
///   proving the dispatch port is genuinely store-agnostic. Requires the worker
///   to have a `[storage]` backend configured.
/// * `LiveKit` — an EGRESS / presentation transport: the producer publishes the
///   channel's frames as a WebRTC video track into a LiveKit room for live
///   in-browser viewing. Unlike the other transports it has **no node-side
///   consumer** — nothing on the net subscribes to it; the only subscriber is a
///   browser viewer that mints a room token from mekhan. Data plane only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelTransport {
    #[default]
    Jetstream,
    NatsLatest,
    S3,
    /// Render `kebab-case` as exactly `livekit` (not `live-kit`) on the wire.
    #[serde(rename = "livekit")]
    LiveKit,
}

impl ChannelTransport {
    /// The wire tag baked into the manifest + descriptor (the dispatch key both
    /// executors and the SDK switch on). Stable across the model→manifest→SDK
    /// boundary — do not derive from the serde rename alone.
    pub fn wire_tag(self) -> &'static str {
        match self {
            ChannelTransport::Jetstream => "jetstream",
            ChannelTransport::NatsLatest => "nats-latest",
            ChannelTransport::S3 => "s3",
            ChannelTransport::LiveKit => "livekit",
        }
    }
}

/// A statically-declared, typed port on an [`AutomatedStep`]. The job emits
/// (`Out`) or reads (`In`) dynamic tokens into/from the channel's synthesized
/// place at runtime; the net wires edges to it by `name`. A control OUT
/// channel lowers uniformly to one accumulating place; the fold discipline
/// lives on the CONSUMER edge's [`ChannelJoin`], NOT here.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub struct Channel {
    pub name: String,
    pub direction: ChannelDirection,
    pub plane: ChannelPlane,
    pub element: ElementType,
    /// Out-of-band transport for a `Data` channel's bytes (default
    /// `Jetstream`). Ignored for `Control` channels. Baked into the manifest so
    /// the producer SDK stamps it into the `open` descriptor and both executors
    /// dispatch the right [`StreamTransport`] adapter off it.
    #[serde(default)]
    pub transport: ChannelTransport,
}
