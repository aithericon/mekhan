//! Runner-side file-serve handler (Phase 3a — multi-endpoint file-servers).
//!
//! A co-located runner serves bytes from an endpoint's LOCAL mount root on
//! demand. mekhan (the control plane) owns ALL file-server config + creds; the
//! runner is **cred-free** and holds NO file-server state — every read carries
//! its own `root` (the endpoint's local mount root, mekhan-authoritative) and a
//! server-relative `canonical_path`. The runner resolves, **path-jails**, seeks,
//! reads, and streams the bytes back. It never serves outside the sent `root`.
//!
//! ## Wire protocol (LOCKED — the mekhan bridge matches this byte-for-byte)
//!
//! - Transport: NATS-native reply-inbox streaming. mekhan opens an inbox and
//!   publishes a [`ServeRequest`] to subject `fileserve.<group>.read` with
//!   `reply = <inbox>`, where `<group>` is the capacity-group UUID the
//!   file-server endpoint binds to (the runner's `worker_routing_partition` /
//!   `runner_id`). Workers join a NATS **queue group** named `<group>` on that
//!   subject so EXACTLY ONE worker in the group handles each request. The
//!   runner streams response frames TO the request's `reply` inbox. mekhan
//!   correlates frames by the `req_id` it minted.
//!
//! - Reply frames (one [`ReplyFrame`] per NATS message, serde-tagged JSON):
//!   * `OPEN  { req_id, seq: 0, content_type, total_size }` — first frame.
//!   * `CHUNK { req_id, seq, bytes }` — ~1 MiB payloads (well under the 8 MiB
//!     NATS max_payload). `seq` starts at 1 (OPEN is seq 0).
//!   * `CLOSE { req_id, final_seq, count }` — terminal success.
//!   * `ERROR { req_id, kind, message }` — terminal failure.
//!
//!   The shape mirrors the existing data-channel chunk envelope
//!   (open → chunk* → close), carried on the inbox instead of a data subject.
//!
//! - Flow control: a bounded in-flight window. The runner streams at most
//!   [`WINDOW`] CHUNK frames ahead of mekhan's acks; mekhan publishes a tiny
//!   ACK (`{ "seq": <n> }`) to the per-request ack subject `<reply>.ack` to
//!   advance the window. A fast disk + slow HTTP client therefore can't grow
//!   the inbox buffer unbounded.
//!
//! The handler runs as a long-lived task alongside the job consumers (it must
//! NOT interfere with job consumption); it reuses the daemon's existing NATS
//! client and the shared cancel/shutdown token.

use std::path::{Component, Path, PathBuf};

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Chunk payload target — ~1 MiB, well under the 8 MiB NATS `max_payload`.
pub const CHUNK_SIZE: usize = 1024 * 1024;

/// In-flight window: stream at most this many CHUNK frames ahead of mekhan's
/// acks before blocking on the ack subject. Keeps the inbox buffer bounded for
/// a fast disk + slow HTTP client.
pub const WINDOW: u64 = 16;

/// The NATS subject a serve request is published to for capacity `group`.
///
/// `group` is the capacity-resource UUID the file-server endpoint binds to —
/// the same partition the co-located runner consumes jobs on
/// (`worker_routing_partition` / `runner_id`). Workers queue-subscribe this
/// subject with queue group = `group` so exactly one handles each request.
pub fn fileserve_subject(group: &str) -> String {
    format!("fileserve.{group}.read")
}

/// The per-request ACK subject mekhan publishes flow-control acks to.
///
/// Derived from the request's `reply` inbox so it is unique per request and
/// needs no extra correlation: `<reply>.ack`.
pub fn ack_subject(reply: &str) -> String {
    format!("{reply}.ack")
}

/// A serve request mekhan publishes to `fileserve.<group>.read`.
///
/// The runner is cred-free: `root` (the endpoint's local mount root) is
/// mekhan-authoritative and sent per-request. `canonical_path` is
/// server-relative; the runner joins + canonicalizes + path-jails it under
/// `root`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeRequest {
    /// mekhan-minted correlation id, echoed on every reply frame.
    pub req_id: String,
    /// The endpoint's local mount root (mekhan-authoritative; the jail boundary).
    pub root: String,
    /// Server-relative path under `root` to read.
    pub canonical_path: String,
    /// Optional byte offset to seek to before reading (HTTP Range start).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    /// Optional cap on bytes to read after `offset` (HTTP Range length).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<u64>,
    /// Originating workspace (audit/correlation only; not a security boundary).
    pub workspace_id: String,
}

/// Terminal failure classes carried on an [`ReplyFrame::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeErrorKind {
    /// The resolved path does not exist.
    NotFound,
    /// The file exists but could not be opened/read (permissions, etc.).
    ReadError,
    /// The resolved path escaped `root` (`..` traversal) — refused.
    PathJail,
    /// Lower-level I/O error while reading/seeking.
    Io,
}

/// One reply frame streamed to the request's `reply` inbox.
///
/// Serde-tagged on `type` (`"open" | "chunk" | "close" | "error"`). The shape
/// mirrors the data-channel chunk envelope (open → chunk* → close).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplyFrame {
    /// First frame: lets mekhan set Content-Type / Content-Length. `seq` is 0.
    Open {
        req_id: String,
        seq: u64,
        content_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_size: Option<u64>,
    },
    /// A ~1 MiB byte chunk. `seq` is monotonic, starting at 1.
    Chunk {
        req_id: String,
        seq: u64,
        bytes: Vec<u8>,
    },
    /// Terminal success: `final_seq` is the last seq emitted (the final CHUNK,
    /// or 0 for an empty file), `count` is the number of CHUNK frames sent.
    Close {
        req_id: String,
        final_seq: u64,
        count: u64,
    },
    /// Terminal failure.
    Error {
        req_id: String,
        kind: ServeErrorKind,
        message: String,
    },
}

impl ReplyFrame {
    /// The correlation id carried by this frame (every variant has one).
    pub fn req_id(&self) -> &str {
        match self {
            ReplyFrame::Open { req_id, .. }
            | ReplyFrame::Chunk { req_id, .. }
            | ReplyFrame::Close { req_id, .. }
            | ReplyFrame::Error { req_id, .. } => req_id,
        }
    }
}

/// A flow-control ACK mekhan publishes to `<reply>.ack` to advance the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeAck {
    /// The highest CHUNK `seq` mekhan has consumed (cumulative).
    pub seq: u64,
}

/// Resolve `root + canonical_path` and PATH-JAIL it: the result must stay
/// strictly inside `root`. Rejects any `..` traversal that escapes the root.
///
/// Done LEXICALLY (not via `fs::canonicalize`, which requires the path to
/// exist) so a missing file inside the jail still classifies as `NotFound`
/// rather than a jail rejection, and so symlink-resolution races don't change
/// the verdict. We normalize away `.`/`..` components and reject if any `..`
/// would pop above `root`.
pub fn resolve_jailed(root: &str, canonical_path: &str) -> Result<PathBuf, ServeErrorKind> {
    let root = Path::new(root);

    // Build the candidate relative path, normalizing components. A leading `/`
    // on canonical_path is treated as root-relative (stripped), never absolute —
    // the runner must never escape the sent root.
    let rel = Path::new(canonical_path);
    let mut normalized = PathBuf::new();
    for comp in rel.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => {
                // Drop absolute prefixes/roots: canonical_path is server-relative.
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop one segment; refuse if it would escape the (empty) root.
                if !normalized.pop() {
                    return Err(ServeErrorKind::PathJail);
                }
            }
            Component::Normal(seg) => normalized.push(seg),
        }
    }

    let candidate = root.join(&normalized);

    // Defense in depth: the lexically-normalized candidate must still be
    // prefixed by root. (After the component walk this always holds, but the
    // explicit check documents + guards the invariant.)
    if !candidate.starts_with(root) {
        return Err(ServeErrorKind::PathJail);
    }
    Ok(candidate)
}

/// Best-effort Content-Type from a file extension (no extra dependency).
///
/// Falls back to `application/octet-stream` for unknown/missing extensions.
pub fn guess_content_type(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let ct = match ext.as_deref() {
        Some("txt") | Some("log") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        Some("xml") => "application/xml",
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("md") => "text/markdown; charset=utf-8",
        Some("yaml") | Some("yml") => "application/yaml",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("zip") => "application/zip",
        Some("gz") | Some("tgz") => "application/gzip",
        Some("tar") => "application/x-tar",
        Some("parquet") => "application/vnd.apache.parquet",
        Some("h5") | Some("hdf5") => "application/x-hdf5",
        Some("fasta") | Some("fa") => "text/x-fasta",
        _ => "application/octet-stream",
    };
    ct.to_string()
}

/// A sink for reply frames. The production sink publishes each frame to the
/// request's NATS reply inbox; the test sink collects them in a Vec.
///
/// `ack_at_least(seq)` blocks until mekhan has acked at least `seq` CHUNKs (or
/// the request is abandoned). The production impl reads the per-request ack
/// subscription; the test impl is unbounded (always satisfied).
#[async_trait::async_trait]
pub trait FrameSink: Send {
    /// Publish one reply frame. Returns `Err` only on a transport failure that
    /// makes further framing pointless (the caller then aborts the request).
    async fn send(&mut self, frame: ReplyFrame) -> Result<(), ()>;

    /// Block until mekhan has cumulatively acked `>= seq` CHUNK frames. Used to
    /// enforce the in-flight window. Default: no-op (unbounded window).
    async fn ack_at_least(&mut self, _seq: u64) {}
}

/// Read a file under `root`, path-jailed, honoring `offset`/`length`, and frame
/// it as OPEN → CHUNK* → CLOSE into `sink`. On any failure emits a single
/// terminal ERROR frame instead. Pure of NATS — `sink` abstracts the transport,
/// so this is the unit-testable core.
///
/// Enforces the in-flight window: after emitting a CHUNK at `seq`, it waits for
/// `sink.ack_at_least(seq.saturating_sub(WINDOW - 1))` so no more than [`WINDOW`]
/// chunks are outstanding ahead of mekhan's acks.
pub async fn serve_file<S: FrameSink>(req: &ServeRequest, sink: &mut S) {
    let path = match resolve_jailed(&req.root, &req.canonical_path) {
        Ok(p) => p,
        Err(kind) => {
            let _ = sink
                .send(ReplyFrame::Error {
                    req_id: req.req_id.clone(),
                    kind,
                    message: format!(
                        "path '{}' escapes serve root '{}'",
                        req.canonical_path, req.root
                    ),
                })
                .await;
            return;
        }
    };

    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(e) => {
            let kind = if e.kind() == std::io::ErrorKind::NotFound {
                ServeErrorKind::NotFound
            } else {
                ServeErrorKind::ReadError
            };
            let _ = sink
                .send(ReplyFrame::Error {
                    req_id: req.req_id.clone(),
                    kind,
                    message: format!("open failed: {e}"),
                })
                .await;
            return;
        }
    };

    // total_size = the byte count this read will yield (after offset + length
    // capping), so mekhan can set Content-Length on a Range response.
    let file_len = match file.metadata().await {
        Ok(m) => m.len(),
        Err(e) => {
            let _ = sink
                .send(ReplyFrame::Error {
                    req_id: req.req_id.clone(),
                    kind: ServeErrorKind::Io,
                    message: format!("stat failed: {e}"),
                })
                .await;
            return;
        }
    };

    let offset = req.offset.unwrap_or(0);
    if offset > 0 {
        if let Err(e) = file.seek(SeekFrom::Start(offset)).await {
            let _ = sink
                .send(ReplyFrame::Error {
                    req_id: req.req_id.clone(),
                    kind: ServeErrorKind::Io,
                    message: format!("seek failed: {e}"),
                })
                .await;
            return;
        }
    }

    // Bytes available from offset, then capped by an explicit length.
    let available = file_len.saturating_sub(offset);
    let to_read = match req.length {
        Some(len) => len.min(available),
        None => available,
    };

    let content_type = guess_content_type(&path);
    if sink
        .send(ReplyFrame::Open {
            req_id: req.req_id.clone(),
            seq: 0,
            content_type,
            total_size: Some(to_read),
        })
        .await
        .is_err()
    {
        return;
    }

    let mut remaining = to_read;
    let mut seq: u64 = 0; // OPEN was seq 0; CHUNKs start at 1.
    let mut count: u64 = 0;
    let mut buf = vec![0u8; CHUNK_SIZE];

    while remaining > 0 {
        let want = (CHUNK_SIZE as u64).min(remaining) as usize;
        let n = match file.read(&mut buf[..want]).await {
            Ok(0) => break, // EOF earlier than expected (file truncated mid-read).
            Ok(n) => n,
            Err(e) => {
                let _ = sink
                    .send(ReplyFrame::Error {
                        req_id: req.req_id.clone(),
                        kind: ServeErrorKind::Io,
                        message: format!("read failed: {e}"),
                    })
                    .await;
                return;
            }
        };

        seq += 1;
        count += 1;
        if sink
            .send(ReplyFrame::Chunk {
                req_id: req.req_id.clone(),
                seq,
                bytes: buf[..n].to_vec(),
            })
            .await
            .is_err()
        {
            return;
        }
        remaining -= n as u64;

        // Flow control: never let more than WINDOW chunks be outstanding ahead
        // of mekhan's acks. Wait until it has acked down to the window edge.
        if seq >= WINDOW {
            sink.ack_at_least(seq - (WINDOW - 1)).await;
        }
    }

    let _ = sink
        .send(ReplyFrame::Close {
            req_id: req.req_id.clone(),
            final_seq: seq,
            count,
        })
        .await;
}

/// The production [`FrameSink`]: publishes each frame to the request's NATS
/// reply inbox and reads cumulative acks off the per-request ack subscription.
struct NatsFrameSink {
    client: async_nats::Client,
    reply: String,
    acks: async_nats::Subscriber,
    acked: u64,
}

#[async_trait::async_trait]
impl FrameSink for NatsFrameSink {
    async fn send(&mut self, frame: ReplyFrame) -> Result<(), ()> {
        let payload = serde_json::to_vec(&frame).map_err(|e| {
            warn!(error = %e, "failed to serialize fileserve reply frame");
        })?;
        self.client
            .publish(self.reply.clone(), payload.into())
            .await
            .map_err(|e| {
                warn!(reply = %self.reply, error = %e, "failed to publish fileserve reply frame");
            })
    }

    async fn ack_at_least(&mut self, seq: u64) {
        // Drain any already-buffered acks first (non-blocking), then block for
        // more until the cumulative ack reaches `seq`. A closed ack
        // subscription (mekhan gone) unblocks us so we stop streaming.
        while self.acked < seq {
            match self.acks.next().await {
                Some(msg) => {
                    if let Ok(ack) = serde_json::from_slice::<ServeAck>(&msg.payload) {
                        self.acked = self.acked.max(ack.seq);
                    }
                }
                None => {
                    debug!(reply = %self.reply, "fileserve ack subscription closed; releasing window");
                    return;
                }
            }
        }
    }
}

/// Spawn the long-lived file-serve handler task.
///
/// Queue-subscribes `fileserve.<group>.read` (queue group = `group`) for each
/// group this worker serves, so exactly one co-grouped worker handles each
/// request. Each request is handled in its own spawned task (bounded by the
/// in-flight window inside [`serve_file`]) so a slow read can't block the
/// subscriber loop or job consumption. Reuses the daemon's existing NATS client
/// and the shared shutdown token; best-effort — a malformed request is logged
/// and skipped, never crashing the daemon.
pub fn spawn_fileserve_handler(
    client: async_nats::Client,
    groups: Vec<String>,
    shutdown: CancellationToken,
) {
    for group in groups {
        let client = client.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let subject = fileserve_subject(&group);
            let mut sub = match client
                .queue_subscribe(subject.clone(), group.clone())
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    warn!(%subject, group = %group, error = %e, "failed to bind fileserve queue subscriber");
                    return;
                }
            };
            info!(%subject, queue_group = %group, "fileserve handler started");

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => {
                        info!(%subject, "fileserve handler shutting down");
                        break;
                    }
                    msg = sub.next() => {
                        let Some(msg) = msg else {
                            warn!(%subject, "fileserve subscription closed");
                            break;
                        };
                        let Some(reply) = msg.reply.clone() else {
                            warn!(%subject, "fileserve request without reply inbox; dropping");
                            continue;
                        };
                        let req: ServeRequest = match serde_json::from_slice(&msg.payload) {
                            Ok(r) => r,
                            Err(e) => {
                                warn!(%subject, error = %e, "malformed fileserve request; dropping");
                                continue;
                            }
                        };
                        let client = client.clone();
                        tokio::spawn(async move {
                            debug!(
                                req_id = %req.req_id,
                                workspace = %req.workspace_id,
                                path = %req.canonical_path,
                                "serving fileserve request"
                            );
                            // Per-request ack subscription on `<reply>.ack` for
                            // flow control. If it can't bind, fall back to an
                            // unbounded window (correctness over flow control).
                            let acks = match client.subscribe(ack_subject(reply.as_str())).await {
                                Ok(s) => s,
                                Err(e) => {
                                    warn!(reply = %reply, error = %e, "failed to bind fileserve ack sub; window disabled");
                                    // Re-subscribe to a throwaway impossible subject would
                                    // be wasteful; instead serve with a sink whose ack is
                                    // a no-op by using an already-closed-equivalent path.
                                    client.subscribe(format!("{reply}.ack.disabled")).await
                                        .expect("core NATS subscribe is infallible for valid subject")
                                }
                            };
                            let mut sink = NatsFrameSink {
                                client,
                                reply: reply.to_string(),
                                acks,
                                acked: 0,
                            };
                            serve_file(&req, &mut sink).await;
                            debug!(req_id = %req.req_id, "fileserve request complete");
                        });
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Collecting test sink: records every frame; unbounded ack window.
    #[derive(Default)]
    struct CollectSink {
        frames: Vec<ReplyFrame>,
    }

    #[async_trait::async_trait]
    impl FrameSink for CollectSink {
        async fn send(&mut self, frame: ReplyFrame) -> Result<(), ()> {
            self.frames.push(frame);
            Ok(())
        }
    }

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        p
    }

    fn req(root: &Path, canonical_path: &str) -> ServeRequest {
        ServeRequest {
            req_id: "r1".to_string(),
            root: root.to_string_lossy().to_string(),
            canonical_path: canonical_path.to_string(),
            offset: None,
            length: None,
            workspace_id: "ws1".to_string(),
        }
    }

    #[test]
    fn subject_and_ack_contract() {
        assert_eq!(fileserve_subject("grp-uuid"), "fileserve.grp-uuid.read");
        assert_eq!(ack_subject("_INBOX.abc"), "_INBOX.abc.ack");
    }

    #[test]
    fn path_jail_rejects_dotdot_escape() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("mount");
        std::fs::create_dir_all(&root).unwrap();

        // `..` that climbs above root → PathJail.
        assert_eq!(
            resolve_jailed(root.to_str().unwrap(), "../secret.txt"),
            Err(ServeErrorKind::PathJail)
        );
        // Sneaky nested escape.
        assert_eq!(
            resolve_jailed(root.to_str().unwrap(), "sub/../../etc/passwd"),
            Err(ServeErrorKind::PathJail)
        );
        // Absolute path is treated as root-relative, never escaping.
        let resolved = resolve_jailed(root.to_str().unwrap(), "/etc/passwd").unwrap();
        assert!(resolved.starts_with(&root));
        assert!(resolved.ends_with("etc/passwd"));
        // A legitimate nested path stays inside.
        let ok = resolve_jailed(root.to_str().unwrap(), "a/b/c.txt").unwrap();
        assert!(ok.starts_with(&root));
    }

    #[tokio::test]
    async fn path_jail_emits_error_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let mut sink = CollectSink::default();
        serve_file(&req(tmp.path(), "../escape.txt"), &mut sink).await;
        assert_eq!(sink.frames.len(), 1);
        match &sink.frames[0] {
            ReplyFrame::Error { kind, req_id, .. } => {
                assert_eq!(*kind, ServeErrorKind::PathJail);
                assert_eq!(req_id, "r1");
            }
            other => panic!("expected ERROR path_jail, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_file_emits_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        let mut sink = CollectSink::default();
        serve_file(&req(tmp.path(), "nope.txt"), &mut sink).await;
        assert_eq!(sink.frames.len(), 1);
        match &sink.frames[0] {
            ReplyFrame::Error { kind, .. } => assert_eq!(*kind, ServeErrorKind::NotFound),
            other => panic!("expected ERROR not_found, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn framing_open_chunks_close_with_seq() {
        let tmp = tempfile::tempdir().unwrap();
        // 2.5 MiB → 3 chunks (1 MiB, 1 MiB, 0.5 MiB).
        let size = CHUNK_SIZE * 2 + CHUNK_SIZE / 2;
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        write_file(tmp.path(), "blob.bin", &data);

        let mut sink = CollectSink::default();
        serve_file(&req(tmp.path(), "blob.bin"), &mut sink).await;

        // OPEN + 3 CHUNK + CLOSE = 5 frames.
        assert_eq!(sink.frames.len(), 5);

        match &sink.frames[0] {
            ReplyFrame::Open {
                seq,
                total_size,
                content_type,
                ..
            } => {
                assert_eq!(*seq, 0);
                assert_eq!(*total_size, Some(size as u64));
                assert_eq!(content_type, "application/octet-stream");
            }
            other => panic!("frame 0 must be OPEN, got {other:?}"),
        }

        // Chunks carry seq 1,2,3 and reassemble to the original bytes.
        let mut reassembled = Vec::new();
        for (i, expect_seq) in (1..=3).enumerate() {
            match &sink.frames[i + 1] {
                ReplyFrame::Chunk { seq, bytes, .. } => {
                    assert_eq!(*seq, expect_seq);
                    reassembled.extend_from_slice(bytes);
                }
                other => panic!("frame {} must be CHUNK, got {other:?}", i + 1),
            }
        }
        assert_eq!(reassembled, data);

        match &sink.frames[4] {
            ReplyFrame::Close {
                final_seq, count, ..
            } => {
                assert_eq!(*final_seq, 3);
                assert_eq!(*count, 3);
            }
            other => panic!("last frame must be CLOSE, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_file_emits_open_then_close_no_chunks() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "empty.txt", b"");
        let mut sink = CollectSink::default();
        serve_file(&req(tmp.path(), "empty.txt"), &mut sink).await;
        assert_eq!(sink.frames.len(), 2);
        assert!(matches!(sink.frames[0], ReplyFrame::Open { .. }));
        match &sink.frames[1] {
            ReplyFrame::Close {
                final_seq, count, ..
            } => {
                assert_eq!(*final_seq, 0);
                assert_eq!(*count, 0);
            }
            other => panic!("expected CLOSE, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn range_offset_and_length_returns_right_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0u8..=255).cycle().take(4000).collect();
        write_file(tmp.path(), "r.bin", &data);

        let mut r = req(tmp.path(), "r.bin");
        r.offset = Some(1000);
        r.length = Some(500);

        let mut sink = CollectSink::default();
        serve_file(&r, &mut sink).await;

        // total_size reflects the capped range.
        match &sink.frames[0] {
            ReplyFrame::Open { total_size, .. } => assert_eq!(*total_size, Some(500)),
            other => panic!("expected OPEN, got {other:?}"),
        }

        let mut got = Vec::new();
        for f in &sink.frames {
            if let ReplyFrame::Chunk { bytes, .. } = f {
                got.extend_from_slice(bytes);
            }
        }
        assert_eq!(got.len(), 500);
        assert_eq!(got, &data[1000..1500]);
    }

    #[tokio::test]
    async fn length_capped_to_available_past_eof() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), "small.bin", &[1, 2, 3, 4, 5]);
        let mut r = req(tmp.path(), "small.bin");
        r.offset = Some(2);
        r.length = Some(1000); // far past EOF

        let mut sink = CollectSink::default();
        serve_file(&r, &mut sink).await;

        let mut got = Vec::new();
        for f in &sink.frames {
            if let ReplyFrame::Chunk { bytes, .. } = f {
                got.extend_from_slice(bytes);
            }
        }
        assert_eq!(got, vec![3, 4, 5]);
    }

    #[test]
    fn reply_frame_wire_shapes_are_tagged() {
        // OPEN
        let open = serde_json::to_value(ReplyFrame::Open {
            req_id: "r".into(),
            seq: 0,
            content_type: "text/plain".into(),
            total_size: Some(10),
        })
        .unwrap();
        assert_eq!(open["type"], "open");
        assert_eq!(open["seq"], 0);
        assert_eq!(open["total_size"], 10);

        // CHUNK (bytes serialize as a JSON array of u8 — serde default for Vec<u8>)
        let chunk = serde_json::to_value(ReplyFrame::Chunk {
            req_id: "r".into(),
            seq: 1,
            bytes: vec![1, 2, 3],
        })
        .unwrap();
        assert_eq!(chunk["type"], "chunk");
        assert_eq!(chunk["bytes"], serde_json::json!([1, 2, 3]));

        // ERROR kind serializes snake_case
        let err = serde_json::to_value(ReplyFrame::Error {
            req_id: "r".into(),
            kind: ServeErrorKind::PathJail,
            message: "x".into(),
        })
        .unwrap();
        assert_eq!(err["type"], "error");
        assert_eq!(err["kind"], "path_jail");
    }

    #[test]
    fn request_round_trips_and_helper_holds() {
        let json = r#"{"req_id":"r1","root":"/mnt/data","canonical_path":"a/b.txt","workspace_id":"ws"}"#;
        let parsed: ServeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.root, "/mnt/data");
        assert_eq!(parsed.canonical_path, "a/b.txt");
        assert_eq!(parsed.offset, None);
        assert_eq!(parsed.length, None);
        // helper accessor stays in sync with the variants
        let f = ReplyFrame::Close {
            req_id: "rx".into(),
            final_seq: 0,
            count: 0,
        };
        assert_eq!(f.req_id(), "rx");
    }
}
