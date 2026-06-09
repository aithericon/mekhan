//! mekhan HTTP serve bridge (docs/32 — multi-endpoint file-servers, Phase 3b).
//!
//! `GET /api/v1/data/entries/{content_hash}/content` resolves a logical entry to
//! its physical copies, picks a servable endpoint, and streams the bytes back to
//! the browser by `access_method`:
//!
//! * **`local_mount`** — the bytes live on a filesystem mount reachable only from
//!   a capacity group's co-located runner. mekhan is cred-free here: it publishes
//!   a [`ServeRequest`] to `fileserve.<group>.read` over the bare NATS client with
//!   a reply inbox, drains the runner's OPEN → CHUNK* → CLOSE (or ERROR) frames,
//!   acks each chunk for flow control, and relays the CHUNK bytes into the HTTP
//!   body. **The wire contract here is LOCKED to the runner-side handler** in
//!   `executor-worker/src/fileserve.rs` (Phase 3a) — the structs below mirror it
//!   byte-for-byte.
//! * **`object_store` / `s3`** — mint a presigned GET URL and 302 the browser
//!   straight to the store (default), or proxy the bytes through mekhan when
//!   `config.proxy_s3_reads` is set. (opendal path — feature `migration-driver`.)
//! * **`sftp`** — stream in-process through opendal. (feature `migration-driver`.)
//!
//! Endpoint SELECTION for this phase is a simple preference order
//! (`local_mount` → `object_store` → `s3` → `sftp`), highest-priority endpoint
//! within the chosen method. Full cost-first routing + verification gating is
//! Phase 5 — see the TODO in [`pick_endpoint`].

use std::time::Duration;

use aithericon_executor_storage::build_operator;
use aithericon_executor_storage_types::{StorageBackend, StorageConfig, StorageCredentials};
use aithericon_secrets::SecretStore;
use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use opendal::Operator;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::file_servers::model::FileServerEndpoint;
use crate::file_servers::queries::ServeCandidate;

// ---------------------------------------------------------------------------
// Wire contract — MUST match executor-worker/src/fileserve.rs (Phase 3a).
// mekhan and the executor live in separate workspaces; these are the
// redeclared mirror structs (serde shapes are the contract, not the types).
// ---------------------------------------------------------------------------

/// The NATS subject a serve request is published to for capacity `group`.
pub fn fileserve_subject(group: &str) -> String {
    format!("fileserve.{group}.read")
}

/// The per-request ACK subject mekhan publishes flow-control acks to.
pub fn ack_subject(reply: &str) -> String {
    format!("{reply}.ack")
}

/// In-flight window: the runner streams at most this many CHUNK frames ahead of
/// mekhan's acks. Mirrors `executor_worker::fileserve::WINDOW`.
pub const WINDOW: u64 = 16;

/// A serve request mekhan publishes to `fileserve.<group>.read`.
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

/// Terminal failure classes carried on a [`ReplyFrame::Error`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServeErrorKind {
    NotFound,
    ReadError,
    PathJail,
    Io,
}

/// One reply frame streamed to the request's `reply` inbox by the runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplyFrame {
    Open {
        req_id: String,
        seq: u64,
        content_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total_size: Option<u64>,
    },
    Chunk {
        req_id: String,
        seq: u64,
        bytes: Vec<u8>,
    },
    Close {
        req_id: String,
        final_seq: u64,
        count: u64,
    },
    Error {
        req_id: String,
        kind: ServeErrorKind,
        message: String,
    },
}

/// A flow-control ACK mekhan publishes to `<reply>.ack` to advance the window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeAck {
    /// The highest CHUNK `seq` mekhan has consumed (cumulative).
    pub seq: u64,
}

// ---------------------------------------------------------------------------
// HTTP Range parsing.
// ---------------------------------------------------------------------------

/// A parsed single-range request: `(offset, length)`. `length == None` means
/// "to end of file". Only `bytes=START-` and `bytes=START-END` forms are
/// honoured (the common browser/media cases); `bytes=-SUFFIX` and multi-range
/// are NOT supported (the serve protocol has no "from the end" primitive — the
/// runner seeks from an absolute offset only), so they fall through to a full
/// read (`None`).
pub fn parse_range(headers: &HeaderMap) -> Option<(u64, Option<u64>)> {
    let raw = headers.get(header::RANGE)?.to_str().ok()?;
    let spec = raw.strip_prefix("bytes=")?;
    // Reject multi-range (comma) and suffix ranges (leading '-').
    if spec.contains(',') || spec.starts_with('-') {
        return None;
    }
    let (start_s, end_s) = spec.split_once('-')?;
    let start: u64 = start_s.trim().parse().ok()?;
    let end_s = end_s.trim();
    if end_s.is_empty() {
        // `bytes=START-` → from START to EOF.
        return Some((start, None));
    }
    let end: u64 = end_s.parse().ok()?;
    if end < start {
        return None;
    }
    // HTTP ranges are inclusive: length = end - start + 1.
    Some((start, Some(end - start + 1)))
}

// ---------------------------------------------------------------------------
// Endpoint selection.
// ---------------------------------------------------------------------------

/// Preference order over `access_method` for this phase. Prefer the cheapest
/// reachable transport: a co-located `local_mount` (no egress) first, then the
/// built-in `object_store`, then external `s3`, then `sftp`.
const METHOD_PREFERENCE: &[&str] = &["local_mount", "object_store", "s3", "sftp"];

/// Pick the endpoint to serve from among a hash's resolved copies.
///
/// Phase-3b policy: walk [`METHOD_PREFERENCE`]; for the first method any
/// candidate offers, return the highest-priority candidate of that method.
/// Candidates arrive already priority-ordered from `serve_candidates`, so the
/// first match per method is the preferred one.
///
/// TODO(Phase 5): full cost-first routing (zone affinity, verification status
/// gating, health) + tie-breaks. For now this is a deterministic static order.
pub fn pick_endpoint(candidates: &[ServeCandidate]) -> Option<&ServeCandidate> {
    for method in METHOD_PREFERENCE {
        if let Some(c) = candidates
            .iter()
            .find(|c| c.endpoint.access_method == *method)
        {
            return Some(c);
        }
    }
    // Fall back to the single highest-priority candidate of any (unknown) method.
    candidates.first()
}

// ---------------------------------------------------------------------------
// Remote endpoints (s3 / sftp): Vault-cred resolution + opendal operator.
// ---------------------------------------------------------------------------

/// Why a remote endpoint couldn't be served / read. Maps cleanly onto HTTP
/// statuses ([`RemoteReadError::into_response`]) and is the error type Phase 4
/// reconcile sees from [`read_remote`].
#[derive(Debug, thiserror::Error)]
pub enum RemoteReadError {
    /// The endpoint has no `resource_ref` but its `access_method` needs one
    /// (external s3 / sftp), or the ref is empty.
    #[error("endpoint {method} has no resource_ref to resolve credentials from")]
    MissingResourceRef { method: String },
    /// The `resource_ref` path doesn't resolve to a live resource/version in
    /// this workspace.
    #[error("resource_ref {path:?} did not resolve in this workspace")]
    ResourceNotFound { path: String },
    /// A required credential / config field was absent from Vault or the
    /// version's `public_config`.
    #[error("missing field {field:?} for endpoint resource {path:?}")]
    MissingField { path: String, field: String },
    /// Reading the secret from the backend failed (Vault unreachable / denied).
    #[error("secret read failed for {field:?}: {source}")]
    Secret {
        field: String,
        #[source]
        source: aithericon_secrets::SecretError,
    },
    /// The opendal operator could not be built (bad config / missing feature).
    #[error("operator build failed: {0}")]
    Operator(String),
    /// The object/file was not found on the backend.
    #[error("object not found at {0:?}")]
    NotFound(String),
    /// Any other backend I/O failure.
    #[error("remote read failed: {0}")]
    Io(String),
    /// A DB error resolving the resource row.
    #[error("database error: {0}")]
    Db(String),
}

impl RemoteReadError {
    /// Map to the HTTP status the serve handler should return when the failure
    /// happens BEFORE any byte is sent.
    pub fn status(&self) -> StatusCode {
        match self {
            RemoteReadError::MissingResourceRef { .. }
            | RemoteReadError::ResourceNotFound { .. }
            | RemoteReadError::NotFound(_) => StatusCode::NOT_FOUND,
            RemoteReadError::MissingField { .. } => StatusCode::INTERNAL_SERVER_ERROR,
            RemoteReadError::Secret { .. }
            | RemoteReadError::Operator(_)
            | RemoteReadError::Io(_)
            | RemoteReadError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn into_response(self) -> Response {
        let status = self.status();
        let msg = self.to_string();
        if status == StatusCode::NOT_FOUND {
            crate::models::error::ApiError::not_found(msg).into_response()
        } else {
            crate::models::error::ApiError::internal(msg).into_response()
        }
    }
}

/// The resolved credential/config envelope for a remote endpoint, in the
/// `StorageConfig` shape the file-ops backend's `build_operator` consumes.
///
/// For `s3` the resource carries `endpoint`/`region`/`bucket`/`access_key_id`/
/// `secret_access_key` (+ optional `force_path_style`); the endpoint `root` is a
/// key prefix inside the bucket. For `sftp` the resource carries only auth
/// (`username`/`private_key`/`known_hosts`); the SSH host lives on the endpoint
/// (`config.host`) and the endpoint `root` is the base path on the server.
#[derive(Debug, Clone)]
pub struct RemoteTarget {
    /// The `StorageConfig` to hand to `build_operator`. `prefix` is empty —
    /// the per-read path is composed explicitly via [`RemoteTarget::object_path`].
    pub storage: StorageConfig,
    /// Logical root inside the backend (bucket key prefix for s3; base path for
    /// sftp). Joined with the copy's server-relative path at read time.
    pub root: String,
}

impl RemoteTarget {
    /// Compose the full backend path for a copy's server-relative
    /// `canonical_path` under this endpoint's `root`, normalising the single
    /// boundary slash. `root` empty → the path verbatim.
    pub fn object_path(&self, canonical_path: &str) -> String {
        let root = self.root.trim_end_matches('/');
        let rel = canonical_path.trim_start_matches('/');
        if root.is_empty() {
            rel.to_string()
        } else {
            format!("{root}/{rel}")
        }
    }
}

/// One field read from a resource version: either a public (non-secret) value
/// taken from `public_config`, or a secret read from Vault via the version's
/// `vault_path`.
enum FieldSource {
    /// Public field name — looked up in `public_config`.
    Public(&'static str),
    /// Secret field name — read as `<vault_path>#<field>`.
    Secret(&'static str),
}

/// Resolve an endpoint's `resource_ref` (a resource `path`) into a
/// [`RemoteTarget`] by joining the resource row → its latest version's
/// `vault_path` + `public_config`, then reading each credential field from the
/// secret store. `access_method` selects the field set (`s3` vs `sftp`).
///
/// This is the cred-chain Phase 3 left unwired: it does for the serve/reconcile
/// read path what `resource_resolver` does for the publish path, but it reads
/// the ACTUAL secret values (the resolver only emits `{{secret:…}}` templates
/// for the engine to expand at firing time).
pub async fn resolve_remote_target(
    db: &PgPool,
    secrets: &dyn SecretStore,
    workspace_id: Uuid,
    endpoint: &FileServerEndpoint,
) -> Result<RemoteTarget, RemoteReadError> {
    let resource_ref = endpoint
        .resource_ref
        .as_deref()
        .filter(|r| !r.is_empty())
        .ok_or_else(|| RemoteReadError::MissingResourceRef {
            method: endpoint.access_method.clone(),
        })?;

    // resource path → (id, latest_version) within the workspace (live only).
    let row: Option<(Uuid, i32)> = sqlx::query_as(
        "SELECT id, latest_version FROM resources \
         WHERE workspace_id = $1 AND path = $2 AND deleted_at IS NULL",
    )
    .bind(workspace_id)
    .bind(resource_ref)
    .fetch_optional(db)
    .await
    .map_err(|e| RemoteReadError::Db(e.to_string()))?;

    let (resource_id, version) = row.ok_or_else(|| RemoteReadError::ResourceNotFound {
        path: resource_ref.to_string(),
    })?;

    // (vault_path, public_config) for the pinned version.
    let vrow: Option<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT vault_path, public_config FROM resource_versions \
         WHERE resource_id = $1 AND version = $2",
    )
    .bind(resource_id)
    .bind(version)
    .fetch_optional(db)
    .await
    .map_err(|e| RemoteReadError::Db(e.to_string()))?;

    let (vault_path, public_config) =
        vrow.ok_or_else(|| RemoteReadError::ResourceNotFound {
            path: resource_ref.to_string(),
        })?;

    // Read one field — public from `public_config`, secret from Vault.
    let read_field =
        |src: FieldSource| -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, RemoteReadError>> + Send>> {
            let public_config = public_config.clone();
            let vault_path = vault_path.clone();
            let path = resource_ref.to_string();
            Box::pin(async move {
                match src {
                    FieldSource::Public(name) => public_config
                        .get(name)
                        .map(|v| match v {
                            serde_json::Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .ok_or(RemoteReadError::MissingField {
                            path,
                            field: name.to_string(),
                        }),
                    FieldSource::Secret(name) => {
                        let key = format!("{vault_path}#{name}");
                        secrets.get(&key).await.map_err(|e| match e {
                            aithericon_secrets::SecretError::NotFound(_) => {
                                RemoteReadError::MissingField {
                                    path,
                                    field: name.to_string(),
                                }
                            }
                            source => RemoteReadError::Secret {
                                field: name.to_string(),
                                source,
                            },
                        })
                    }
                }
            })
        };

    match endpoint.access_method.as_str() {
        "s3" => {
            let s3_endpoint = read_field(FieldSource::Public("endpoint")).await?;
            let region = read_field(FieldSource::Public("region")).await?;
            let bucket = read_field(FieldSource::Public("bucket")).await?;
            let access_key = read_field(FieldSource::Secret("access_key_id")).await?;
            let secret_key = read_field(FieldSource::Secret("secret_access_key")).await?;

            let storage = StorageConfig {
                backend: StorageBackend::S3,
                endpoint: s3_endpoint,
                bucket,
                region: Some(region),
                prefix: String::new(),
                credentials: StorageCredentials {
                    access_key,
                    secret_key,
                },
                retry: Default::default(),
                resource_alias: None,
            };
            Ok(RemoteTarget {
                storage,
                root: endpoint.root.clone(),
            })
        }
        "sftp" => {
            let username = read_field(FieldSource::Public("username")).await?;
            let private_key = read_field(FieldSource::Secret("private_key")).await?;
            // known_hosts policy is optional; default "Accept" (matches the
            // file-ops backend's sftp builder default for a curated NAS).
            let strategy = public_config
                .get("known_hosts")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(capitalize_strategy)
                .unwrap_or_else(|| "Accept".to_string());
            // The SSH host lives on the ENDPOINT (the sftp resource holds auth
            // only). `config.host` is the canonical key; fall back to `config.endpoint`.
            let host = endpoint
                .config
                .get("host")
                .or_else(|| endpoint.config.get("endpoint"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| RemoteReadError::MissingField {
                    path: resource_ref.to_string(),
                    field: "config.host".to_string(),
                })?
                .to_string();

            let storage = StorageConfig {
                backend: StorageBackend::Sftp,
                endpoint: host,
                bucket: String::new(),
                // `region` doubles as the known-hosts strategy in the file-ops
                // sftp builder (see executor-storage::build_operator).
                region: Some(strategy),
                prefix: String::new(),
                credentials: StorageCredentials {
                    access_key: username,
                    secret_key: private_key,
                },
                retry: Default::default(),
                resource_alias: None,
            };
            Ok(RemoteTarget {
                storage,
                // The sftp operator roots at "/", so the endpoint root is part
                // of the per-read path (see RemoteTarget::object_path).
                root: endpoint.root.clone(),
            })
        }
        other => Err(RemoteReadError::Operator(format!(
            "resolve_remote_target called for non-remote access_method {other:?}"
        ))),
    }
}

/// Normalise a known-hosts policy string from `public_config` into the
/// PascalCase variant opendal's sftp builder expects (`Strict`/`Accept`/`Add`).
fn capitalize_strategy(s: &str) -> String {
    let lower = s.trim().to_ascii_lowercase();
    match lower.as_str() {
        "strict" => "Strict".to_string(),
        "add" => "Add".to_string(),
        // Anything else (incl. "accept") → Accept (the pragmatic default).
        _ => "Accept".to_string(),
    }
}

/// The bytes + metadata of a remote read. `stream` yields `io::Result<Bytes>`
/// directly consumable by [`Body::from_stream`].
pub struct RemoteRead {
    pub content_type: String,
    /// Total object size (full object), or `None` when the backend doesn't
    /// report it. For a ranged read this is still the FULL size (the stream is
    /// the capped slice).
    pub size: Option<u64>,
    pub stream: opendal::FuturesBytesStream,
}

/// Build the opendal [`Operator`] for a resolved remote target. Public so Phase
/// 4 reconcile can reuse the exact operator the serve path uses.
pub fn build_remote_operator(target: &RemoteTarget) -> Result<Operator, RemoteReadError> {
    build_operator(&target.storage).map_err(|e| RemoteReadError::Operator(e.to_string()))
}

/// **Reusable remote read** (docs/32 Phase 4 entry point).
///
/// Resolve an endpoint's creds, build the operator, and open a (optionally
/// ranged) byte stream for `canonical_path` — for BOTH `s3` and `sftp`. Returns
/// the content-type, full object size, and a `Stream<Item = io::Result<Bytes>>`.
///
/// Phase 4 reconcile calls this to read + hash a remote endpoint's copy: drain
/// the stream through a hasher. `range = None` reads the whole object;
/// `Some((offset, len))` reads `len` bytes from `offset` (`len = None` → to EOF).
pub async fn read_remote(
    db: &PgPool,
    secrets: &dyn SecretStore,
    workspace_id: Uuid,
    endpoint: &FileServerEndpoint,
    canonical_path: &str,
    range: Option<(u64, Option<u64>)>,
) -> Result<RemoteRead, RemoteReadError> {
    let target = resolve_remote_target(db, secrets, workspace_id, endpoint).await?;
    let operator = build_remote_operator(&target)?;
    let object_path = target.object_path(canonical_path);

    // stat() up front for content-type + total size, and to surface a clean
    // NotFound before we start a body.
    let meta = operator.stat(&object_path).await.map_err(map_opendal_err)?;
    let size = Some(meta.content_length());
    let content_type = meta
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();

    let reader = operator
        .reader_with(&object_path)
        .await
        .map_err(map_opendal_err)?;

    // Translate the parsed HTTP Range into an opendal byte range.
    let total = meta.content_length();
    let stream = match range {
        Some((offset, Some(len))) => {
            let end = offset.saturating_add(len).min(total);
            reader.into_bytes_stream(offset..end).await
        }
        Some((offset, None)) => reader.into_bytes_stream(offset..total).await,
        None => reader.into_bytes_stream(0..total).await,
    }
    .map_err(map_opendal_err)?;

    Ok(RemoteRead {
        content_type,
        size,
        stream,
    })
}

/// Mint a presigned GET URL for a resolved s3 target's object. Only valid for
/// `s3` (opendal sftp has no presign). `expires` caps the URL validity.
pub async fn presign_s3(
    target: &RemoteTarget,
    canonical_path: &str,
    expires: Duration,
) -> Result<String, RemoteReadError> {
    let operator = build_remote_operator(target)?;
    let object_path = target.object_path(canonical_path);
    let req = operator
        .presign_read(&object_path, expires)
        .await
        .map_err(map_opendal_err)?;
    Ok(req.uri().to_string())
}

fn map_opendal_err(e: opendal::Error) -> RemoteReadError {
    match e.kind() {
        opendal::ErrorKind::NotFound => RemoteReadError::NotFound(e.to_string()),
        _ => RemoteReadError::Io(e.to_string()),
    }
}

/// Serve an external `s3` endpoint: presign + 302 by default, or proxy the bytes
/// in-process when `proxy_s3_reads` is set. Mirrors the built-in object_store
/// behaviour but against the endpoint's own resolved creds.
#[allow(clippy::too_many_arguments)]
pub async fn serve_s3_endpoint(
    db: &PgPool,
    secrets: &dyn SecretStore,
    workspace_id: Uuid,
    endpoint: &FileServerEndpoint,
    canonical_path: &str,
    filename: &str,
    proxy: bool,
    range: Option<(u64, Option<u64>)>,
) -> Response {
    let target = match resolve_remote_target(db, secrets, workspace_id, endpoint).await {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };

    if proxy {
        return stream_remote(&target, canonical_path, filename, range).await;
    }

    // Default: presigned 302 — bytes never transit mekhan. (A Range request
    // can't be honoured through a presign redirect; the client re-issues the
    // Range against the store directly, so we still 302.)
    match presign_s3(&target, canonical_path, Duration::from_secs(300)).await {
        Ok(url) => match header::HeaderValue::from_str(&url) {
            Ok(loc) => (StatusCode::FOUND, [(header::LOCATION, loc)]).into_response(),
            Err(_) => {
                crate::models::error::ApiError::internal("presigned URL was not a valid header value")
                    .into_response()
            }
        },
        Err(e) => e.into_response(),
    }
}

/// Serve an `sftp` endpoint: always streamed in-process through opendal (sftp
/// has no presign). Honours Range where opendal supports the ranged read.
pub async fn serve_sftp_endpoint(
    db: &PgPool,
    secrets: &dyn SecretStore,
    workspace_id: Uuid,
    endpoint: &FileServerEndpoint,
    canonical_path: &str,
    filename: &str,
    range: Option<(u64, Option<u64>)>,
) -> Response {
    let target = match resolve_remote_target(db, secrets, workspace_id, endpoint).await {
        Ok(t) => t,
        Err(e) => return e.into_response(),
    };
    stream_remote(&target, canonical_path, filename, range).await
}

/// Shared in-process streamer for a resolved remote target (proxied s3 + sftp).
/// Opens a (ranged) opendal byte stream and relays it into the HTTP body.
async fn stream_remote(
    target: &RemoteTarget,
    canonical_path: &str,
    filename: &str,
    range: Option<(u64, Option<u64>)>,
) -> Response {
    let operator = match build_remote_operator(target) {
        Ok(o) => o,
        Err(e) => return e.into_response(),
    };
    let object_path = target.object_path(canonical_path);

    let meta = match operator.stat(&object_path).await {
        Ok(m) => m,
        Err(e) => return map_opendal_err(e).into_response(),
    };
    let total = meta.content_length();
    let content_type = meta
        .content_type()
        .unwrap_or("application/octet-stream")
        .to_string();

    let reader = match operator.reader_with(&object_path).await {
        Ok(r) => r,
        Err(e) => return map_opendal_err(e).into_response(),
    };

    // The number of bytes the body will carry (Content-Length).
    let (body_len, byte_range) = match range {
        Some((offset, Some(len))) => {
            let end = offset.saturating_add(len).min(total);
            (end.saturating_sub(offset), offset..end)
        }
        Some((offset, None)) => (total.saturating_sub(offset), offset..total),
        None => (total, 0..total),
    };

    let stream = match reader.into_bytes_stream(byte_range).await {
        Ok(s) => s,
        Err(e) => return map_opendal_err(e).into_response(),
    };

    let mut resp_headers = HeaderMap::new();
    if let Ok(v) = header::HeaderValue::from_str(&content_type) {
        resp_headers.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = header::HeaderValue::from_str(&body_len.to_string()) {
        resp_headers.insert(header::CONTENT_LENGTH, v);
    }
    resp_headers.insert(header::ACCEPT_RANGES, header::HeaderValue::from_static("bytes"));
    let disposition = format!("inline; filename=\"{}\"", sanitize_filename(filename));
    if let Ok(v) = header::HeaderValue::from_str(&disposition) {
        resp_headers.insert(header::CONTENT_DISPOSITION, v);
    }

    let status = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    (status, resp_headers, Body::from_stream(stream)).into_response()
}

// ---------------------------------------------------------------------------
// local_mount: NATS relay.
// ---------------------------------------------------------------------------

/// How long to wait for the runner's first reply frame (OPEN/ERROR) before
/// giving up with 504. A co-located runner answers in milliseconds; a generous
/// cap covers a momentarily-busy group.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(15);
/// Idle timeout between subsequent frames once streaming has begun.
const FRAME_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Serve a `local_mount` copy by relaying the runner's NATS frame stream into
/// the HTTP body.
///
/// Resolves the endpoint's `group_id`, publishes a [`ServeRequest`] to
/// `fileserve.<group>.read` with a fresh reply inbox, and:
///
/// 1. Awaits the first frame. ERROR before any byte → 404 (not_found/path_jail)
///    or 500 (io/read_error). OPEN → set Content-Type / Content-Length and begin
///    streaming.
/// 2. Streams CHUNK bytes into [`Body::from_stream`], acking each consumed chunk
///    on `<reply>.ack` to keep the runner's in-flight window open. A mid-stream
///    ERROR aborts the body (the bytes already sent can't be unsent).
///
/// `filename` is used only for the `Content-Disposition` header.
pub async fn serve_local_mount(
    client: &async_nats::Client,
    endpoint: &FileServerEndpoint,
    server_relative_path: &str,
    filename: &str,
    workspace_id: &str,
    range: Option<(u64, Option<u64>)>,
) -> Response {
    let Some(group) = endpoint.group_id.as_deref().filter(|g| !g.is_empty()) else {
        return crate::models::error::ApiError::internal(
            "local_mount endpoint has no group_id to dispatch to",
        )
        .into_response();
    };

    let req_id = uuid::Uuid::new_v4().to_string();
    let reply = client.new_inbox();
    let (offset, length) = match range {
        Some((o, l)) => (Some(o), l),
        None => (None, None),
    };

    // Subscribe to the reply inbox BEFORE publishing so no frame is missed.
    let mut frames = match client.subscribe(reply.clone()).await {
        Ok(s) => s,
        Err(e) => {
            return crate::models::error::ApiError::internal(format!(
                "failed to subscribe serve reply inbox: {e}"
            ))
            .into_response();
        }
    };

    let request = ServeRequest {
        req_id: req_id.clone(),
        root: endpoint.root.clone(),
        canonical_path: server_relative_path.to_string(),
        offset,
        length,
        workspace_id: workspace_id.to_string(),
    };
    let payload = match serde_json::to_vec(&request) {
        Ok(p) => p,
        Err(e) => {
            return crate::models::error::ApiError::internal(format!(
                "failed to serialize serve request: {e}"
            ))
            .into_response();
        }
    };

    let subject = fileserve_subject(group);
    if let Err(e) = client
        .publish_with_reply(subject.clone(), reply.clone(), payload.into())
        .await
    {
        return crate::models::error::ApiError::internal(format!(
            "failed to publish serve request: {e}"
        ))
        .into_response();
    }

    // (1) First frame: OPEN (begin streaming) or ERROR (terminal, no body yet).
    let first = match tokio::time::timeout(FIRST_FRAME_TIMEOUT, frames.next_frame()).await {
        Ok(Some(f)) => f,
        Ok(None) => {
            return crate::models::error::ApiError::service_unavailable(
                "serve reply stream closed before any frame",
            )
            .into_response();
        }
        Err(_) => {
            return crate::models::error::ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timed out waiting for serving runner (group has no live worker?)",
            )
            .into_response();
        }
    };

    let (content_type, total_size) = match first {
        ReplyFrame::Open {
            content_type,
            total_size,
            ..
        } => (content_type, total_size),
        ReplyFrame::Error { kind, message, .. } => {
            return error_frame_to_response(kind, &message);
        }
        // CHUNK/CLOSE before OPEN is a protocol violation by the runner.
        other => {
            return crate::models::error::ApiError::internal(format!(
                "serve protocol error: expected OPEN, got {other:?}"
            ))
            .into_response();
        }
    };

    // (2) Stream the remaining frames (CHUNK* → CLOSE) into the body, acking as
    // we consume. A mid-stream ERROR truncates the body (best effort — the bytes
    // already flushed to the client cannot be recalled).
    let ack_subject = ack_subject(&reply);
    let client = client.clone();
    let stream = async_stream::stream! {
        let mut consumed: u64 = 0;
        loop {
            match tokio::time::timeout(FRAME_IDLE_TIMEOUT, frames.next_frame()).await {
                Ok(Some(ReplyFrame::Chunk { seq, bytes, .. })) => {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(bytes));
                    consumed = consumed.max(seq);
                    // Cumulative ack — keep the runner's WINDOW open. Cheap; the
                    // runner takes max() so dups/out-of-order are harmless.
                    let ack = serde_json::to_vec(&ServeAck { seq: consumed })
                        .expect("ServeAck serializes");
                    let _ = client.publish(ack_subject.clone(), ack.into()).await;
                }
                Ok(Some(ReplyFrame::Close { .. })) => break,
                Ok(Some(ReplyFrame::Error { kind, message, .. })) => {
                    tracing::warn!(?kind, %message, "serve mid-stream error; truncating body");
                    yield Err(std::io::Error::other(format!("serve error: {message}")));
                    break;
                }
                Ok(Some(ReplyFrame::Open { .. })) => {
                    // A second OPEN is a protocol error; stop.
                    break;
                }
                Ok(None) => break, // inbox closed
                Err(_) => {
                    // Idle: producer gone without CLOSE.
                    yield Err(std::io::Error::other("serve stream idle (runner gone)"));
                    break;
                }
            }
        }
    };

    let mut resp_headers = HeaderMap::new();
    if let Ok(v) = header::HeaderValue::from_str(&content_type) {
        resp_headers.insert(header::CONTENT_TYPE, v);
    }
    if let Some(size) = total_size {
        if let Ok(v) = header::HeaderValue::from_str(&size.to_string()) {
            resp_headers.insert(header::CONTENT_LENGTH, v);
        }
    }
    resp_headers.insert(header::ACCEPT_RANGES, header::HeaderValue::from_static("bytes"));
    let disposition = format!("inline; filename=\"{}\"", sanitize_filename(filename));
    if let Ok(v) = header::HeaderValue::from_str(&disposition) {
        resp_headers.insert(header::CONTENT_DISPOSITION, v);
    }

    // A Range request that produced a capped read gets 206; a full read gets 200.
    let status = if range.is_some() {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };

    (status, resp_headers, Body::from_stream(stream)).into_response()
}

/// Why a `local_mount` read (the NATS relay) couldn't be completed. Distinct
/// from [`RemoteReadError`] because the transport (NATS frames) is different,
/// but it carries the same key distinction Phase-4 reconcile needs: `NotFound`
/// (the endpoint lacks the file — a coverage gap, NOT a failure) vs everything
/// else (a real read/transport error).
#[derive(Debug, thiserror::Error)]
pub enum LocalReadError {
    /// The endpoint has no `group_id` to dispatch the serve request to.
    #[error("local_mount endpoint has no group_id to dispatch to")]
    NoGroup,
    /// The runner reported the file is absent (not_found / path_jail) — the
    /// endpoint does not cover this canonical path.
    #[error("file not found on local_mount: {0}")]
    NotFound(String),
    /// A read / IO error on the runner side, or a transport/timeout/protocol
    /// failure draining the reply stream.
    #[error("local_mount read failed: {0}")]
    Io(String),
}

/// **Reusable local_mount read** (docs/32 Phase 4 entry point, mirror of
/// [`read_remote`] for the NATS transport).
///
/// Issue the *same* [`ServeRequest`] `serve_local_mount` uses to
/// `fileserve.<group>.read`, but instead of relaying frames into an HTTP body,
/// drain OPEN → CHUNK* → CLOSE and COLLECT the bytes into a `Vec<u8>`. Acks each
/// chunk for flow control exactly as the serve path does (the protocol is the
/// same — this does NOT duplicate it, it shares the frame helpers). A
/// `not_found` / `path_jail` ERROR maps to [`LocalReadError::NotFound`] so the
/// caller can treat it as a coverage gap rather than a failure.
///
/// Always reads the WHOLE object (no range) — reconcile must hash full bytes.
pub async fn read_local_bytes(
    client: &async_nats::Client,
    group: &str,
    root: &str,
    canonical_path: &str,
    workspace_id: &str,
) -> Result<Vec<u8>, LocalReadError> {
    if group.is_empty() {
        return Err(LocalReadError::NoGroup);
    }

    let req_id = uuid::Uuid::new_v4().to_string();
    let reply = client.new_inbox();

    // Subscribe BEFORE publishing so no frame is missed.
    let mut frames = client
        .subscribe(reply.clone())
        .await
        .map_err(|e| LocalReadError::Io(format!("subscribe reply inbox: {e}")))?;

    let request = ServeRequest {
        req_id: req_id.clone(),
        root: root.to_string(),
        canonical_path: canonical_path.to_string(),
        offset: None,
        length: None,
        workspace_id: workspace_id.to_string(),
    };
    let payload =
        serde_json::to_vec(&request).map_err(|e| LocalReadError::Io(format!("serialize: {e}")))?;

    let subject = fileserve_subject(group);
    client
        .publish_with_reply(subject, reply.clone(), payload.into())
        .await
        .map_err(|e| LocalReadError::Io(format!("publish: {e}")))?;

    // (1) First frame: OPEN (begin) or ERROR (terminal).
    let first = match tokio::time::timeout(FIRST_FRAME_TIMEOUT, frames.next_frame()).await {
        Ok(Some(f)) => f,
        Ok(None) => return Err(LocalReadError::Io("reply stream closed before any frame".into())),
        Err(_) => {
            return Err(LocalReadError::Io(
                "timed out waiting for serving runner (group has no live worker?)".into(),
            ))
        }
    };
    match first {
        ReplyFrame::Open { .. } => {}
        ReplyFrame::Error { kind, message, .. } => {
            return Err(local_error_frame(kind, &message));
        }
        other => {
            return Err(LocalReadError::Io(format!(
                "serve protocol error: expected OPEN, got {other:?}"
            )))
        }
    }

    // (2) Drain CHUNK* → CLOSE, acking each consumed chunk.
    let ack_subject = ack_subject(&reply);
    let mut buf: Vec<u8> = Vec::new();
    let mut consumed: u64 = 0;
    loop {
        match tokio::time::timeout(FRAME_IDLE_TIMEOUT, frames.next_frame()).await {
            Ok(Some(ReplyFrame::Chunk { seq, bytes, .. })) => {
                buf.extend_from_slice(&bytes);
                consumed = consumed.max(seq);
                let ack =
                    serde_json::to_vec(&ServeAck { seq: consumed }).expect("ServeAck serializes");
                let _ = client.publish(ack_subject.clone(), ack.into()).await;
            }
            Ok(Some(ReplyFrame::Close { .. })) => break,
            Ok(Some(ReplyFrame::Error { kind, message, .. })) => {
                return Err(local_error_frame(kind, &message));
            }
            // A second OPEN is a protocol error; stop.
            Ok(Some(ReplyFrame::Open { .. })) => {
                return Err(LocalReadError::Io("unexpected second OPEN frame".into()))
            }
            Ok(None) => return Err(LocalReadError::Io("reply stream closed mid-read".into())),
            Err(_) => return Err(LocalReadError::Io("serve stream idle (runner gone)".into())),
        }
    }
    Ok(buf)
}

/// Map a terminal ERROR frame to a [`LocalReadError`] — `not_found`/`path_jail`
/// become `NotFound` (coverage gap), the rest become `Io`.
fn local_error_frame(kind: ServeErrorKind, message: &str) -> LocalReadError {
    match kind {
        ServeErrorKind::NotFound | ServeErrorKind::PathJail => {
            LocalReadError::NotFound(message.to_string())
        }
        ServeErrorKind::ReadError | ServeErrorKind::Io => LocalReadError::Io(message.to_string()),
    }
}

/// Map a pre-first-byte ERROR frame to the right HTTP status.
fn error_frame_to_response(kind: ServeErrorKind, message: &str) -> Response {
    match kind {
        ServeErrorKind::NotFound | ServeErrorKind::PathJail => {
            crate::models::error::ApiError::not_found(format!("file not servable: {message}"))
                .into_response()
        }
        ServeErrorKind::ReadError | ServeErrorKind::Io => {
            crate::models::error::ApiError::internal(format!("serve read failed: {message}"))
                .into_response()
        }
    }
}

/// Strip characters that would break a quoted `Content-Disposition` filename
/// (quotes, control chars, path separators). Conservative — falls back to a
/// generic name when nothing usable remains.
fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\' && *c != '/')
        .collect();
    if cleaned.trim().is_empty() {
        "download".to_string()
    } else {
        cleaned
    }
}

/// Thin helper trait: pull the next typed [`ReplyFrame`] off a NATS subscriber,
/// skipping malformed payloads (logged, not fatal — a corrupt frame from a
/// rogue publisher must not wedge the stream).
trait NextFrame {
    async fn next_frame(&mut self) -> Option<ReplyFrame>;
}

impl NextFrame for async_nats::Subscriber {
    async fn next_frame(&mut self) -> Option<ReplyFrame> {
        use futures::StreamExt;
        loop {
            let msg = self.next().await?;
            match serde_json::from_slice::<ReplyFrame>(&msg.payload) {
                Ok(f) => return Some(f),
                Err(e) => {
                    tracing::warn!(error = %e, "skipping malformed serve reply frame");
                    continue;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_servers::model::FileServerEndpoint;
    use chrono::Utc;

    fn ep(method: &str, priority: i32) -> FileServerEndpoint {
        FileServerEndpoint {
            id: uuid::Uuid::new_v4(),
            file_server_id: uuid::Uuid::new_v4(),
            access_method: method.to_string(),
            root: "/mnt/data".to_string(),
            resource_ref: None,
            group_id: Some("grp-1".to_string()),
            status: "online".to_string(),
            verification_status: "verified".to_string(),
            last_verified: None,
            last_seen: None,
            priority,
            config: serde_json::json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn cand(method: &str, priority: i32) -> ServeCandidate {
        ServeCandidate {
            path: "a/b.txt".to_string(),
            endpoint: ep(method, priority),
        }
    }

    #[test]
    fn subject_and_ack_match_runner_contract() {
        assert_eq!(fileserve_subject("grp-uuid"), "fileserve.grp-uuid.read");
        assert_eq!(ack_subject("_INBOX.abc"), "_INBOX.abc.ack");
    }

    #[test]
    fn request_omits_offset_length_when_none() {
        let req = ServeRequest {
            req_id: "r1".into(),
            root: "/mnt".into(),
            canonical_path: "a.txt".into(),
            offset: None,
            length: None,
            workspace_id: "ws".into(),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("offset").is_none(), "offset must be absent when None");
        assert!(v.get("length").is_none(), "length must be absent when None");
    }

    #[test]
    fn request_includes_offset_length_when_set() {
        let req = ServeRequest {
            req_id: "r1".into(),
            root: "/mnt".into(),
            canonical_path: "a.txt".into(),
            offset: Some(100),
            length: Some(50),
            workspace_id: "ws".into(),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["offset"], 100);
        assert_eq!(v["length"], 50);
    }

    #[test]
    fn reply_frame_open_parses_from_runner_shape() {
        let json = r#"{"type":"open","req_id":"r","seq":0,"content_type":"text/plain","total_size":42}"#;
        let f: ReplyFrame = serde_json::from_str(json).unwrap();
        match f {
            ReplyFrame::Open {
                total_size,
                content_type,
                seq,
                ..
            } => {
                assert_eq!(seq, 0);
                assert_eq!(total_size, Some(42));
                assert_eq!(content_type, "text/plain");
            }
            other => panic!("expected OPEN, got {other:?}"),
        }
    }

    #[test]
    fn reply_frame_chunk_bytes_are_u8_array() {
        let json = r#"{"type":"chunk","req_id":"r","seq":1,"bytes":[1,2,3]}"#;
        let f: ReplyFrame = serde_json::from_str(json).unwrap();
        match f {
            ReplyFrame::Chunk { seq, bytes, .. } => {
                assert_eq!(seq, 1);
                assert_eq!(bytes, vec![1u8, 2, 3]);
            }
            other => panic!("expected CHUNK, got {other:?}"),
        }
    }

    #[test]
    fn reply_frame_error_kind_snake_case() {
        let json = r#"{"type":"error","req_id":"r","kind":"path_jail","message":"x"}"#;
        let f: ReplyFrame = serde_json::from_str(json).unwrap();
        match f {
            ReplyFrame::Error { kind, .. } => assert_eq!(kind, ServeErrorKind::PathJail),
            other => panic!("expected ERROR, got {other:?}"),
        }
    }

    #[test]
    fn ack_serializes_to_seq_only() {
        let v = serde_json::to_value(ServeAck { seq: 7 }).unwrap();
        assert_eq!(v, serde_json::json!({"seq": 7}));
    }

    #[test]
    fn range_open_ended() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=1000-".parse().unwrap());
        assert_eq!(parse_range(&h), Some((1000, None)));
    }

    #[test]
    fn range_closed_is_inclusive_length() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=0-499".parse().unwrap());
        // 0..=499 inclusive → 500 bytes.
        assert_eq!(parse_range(&h), Some((0, Some(500))));
    }

    #[test]
    fn range_suffix_and_multirange_unsupported() {
        let mut h = HeaderMap::new();
        h.insert(header::RANGE, "bytes=-500".parse().unwrap());
        assert_eq!(parse_range(&h), None);
        let mut h2 = HeaderMap::new();
        h2.insert(header::RANGE, "bytes=0-1,2-3".parse().unwrap());
        assert_eq!(parse_range(&h2), None);
    }

    #[test]
    fn range_absent_is_none() {
        let h = HeaderMap::new();
        assert_eq!(parse_range(&h), None);
    }

    #[test]
    fn pick_prefers_local_mount_then_object_store() {
        let cands = vec![cand("sftp", 100), cand("object_store", 5), cand("local_mount", 1)];
        // local_mount wins despite lowest priority — it's the cheapest transport.
        assert_eq!(pick_endpoint(&cands).unwrap().endpoint.access_method, "local_mount");
    }

    #[test]
    fn pick_falls_through_method_order() {
        let cands = vec![cand("sftp", 50), cand("s3", 10)];
        // No local_mount/object_store → s3 beats sftp.
        assert_eq!(pick_endpoint(&cands).unwrap().endpoint.access_method, "s3");
    }

    #[test]
    fn pick_highest_priority_within_method() {
        // Two object_store endpoints; candidates arrive priority-ordered so the
        // first match (priority 9) wins.
        let cands = vec![cand("object_store", 9), cand("object_store", 1)];
        assert_eq!(pick_endpoint(&cands).unwrap().endpoint.priority, 9);
    }

    #[test]
    fn pick_empty_is_none() {
        assert!(pick_endpoint(&[]).is_none());
    }

    #[test]
    fn sanitize_filename_strips_unsafe() {
        assert_eq!(sanitize_filename("a/b\"c.txt"), "abc.txt");
        assert_eq!(sanitize_filename(""), "download");
        assert_eq!(sanitize_filename("///"), "download");
    }

    #[test]
    fn error_frame_status_mapping() {
        use axum::response::IntoResponse;
        let r = error_frame_to_response(ServeErrorKind::NotFound, "nope");
        assert_eq!(r.into_response().status(), StatusCode::NOT_FOUND);
        let r = error_frame_to_response(ServeErrorKind::PathJail, "esc");
        assert_eq!(r.into_response().status(), StatusCode::NOT_FOUND);
        let r = error_frame_to_response(ServeErrorKind::Io, "io");
        assert_eq!(r.into_response().status(), StatusCode::INTERNAL_SERVER_ERROR);
        let r = error_frame_to_response(ServeErrorKind::ReadError, "rd");
        assert_eq!(r.into_response().status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // -----------------------------------------------------------------------
    // Remote (s3 / sftp) cred-resolution + presign + branch selection.
    // -----------------------------------------------------------------------

    fn s3_target(root: &str) -> RemoteTarget {
        RemoteTarget {
            storage: StorageConfig {
                backend: StorageBackend::S3,
                endpoint: "http://minio.local:9000".into(),
                bucket: "lab-data".into(),
                region: Some("us-east-1".into()),
                prefix: String::new(),
                credentials: StorageCredentials {
                    access_key: "AKIAEXAMPLE".into(),
                    secret_key: "secret".into(),
                },
                retry: Default::default(),
                resource_alias: None,
            },
            root: root.to_string(),
        }
    }

    #[test]
    fn object_path_joins_root_and_relative_with_single_slash() {
        let t = s3_target("datasets/2024");
        assert_eq!(t.object_path("runs/a.h5"), "datasets/2024/runs/a.h5");
        // Trailing root slash + leading path slash collapse to one.
        let t = s3_target("datasets/2024/");
        assert_eq!(t.object_path("/runs/a.h5"), "datasets/2024/runs/a.h5");
    }

    #[test]
    fn object_path_empty_root_is_verbatim() {
        let t = s3_target("");
        assert_eq!(t.object_path("runs/a.h5"), "runs/a.h5");
        assert_eq!(t.object_path("/runs/a.h5"), "runs/a.h5");
    }

    #[test]
    fn capitalize_strategy_normalises_known_hosts_policy() {
        assert_eq!(capitalize_strategy("strict"), "Strict");
        assert_eq!(capitalize_strategy("STRICT"), "Strict");
        assert_eq!(capitalize_strategy("add"), "Add");
        assert_eq!(capitalize_strategy("accept"), "Accept");
        // Unknown / empty → the pragmatic Accept default.
        assert_eq!(capitalize_strategy("whatever"), "Accept");
        assert_eq!(capitalize_strategy(""), "Accept");
    }

    #[test]
    fn remote_read_error_status_mapping() {
        assert_eq!(
            RemoteReadError::MissingResourceRef { method: "s3".into() }.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            RemoteReadError::ResourceNotFound { path: "p".into() }.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            RemoteReadError::NotFound("x".into()).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            RemoteReadError::MissingField {
                path: "p".into(),
                field: "bucket".into()
            }
            .status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            RemoteReadError::Operator("boom".into()).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    /// Presigning is local request-signing (no network) — exercising the s3
    /// presign path against a static target proves the operator builds and the
    /// minted URL embeds the object key + bucket + a signature query.
    #[tokio::test]
    async fn presign_s3_mints_signed_url_for_object_key() {
        let t = s3_target("datasets");
        let url = presign_s3(&t, "runs/a.h5", Duration::from_secs(120))
            .await
            .expect("presign mints a URL");
        // Path-style endpoint → host + bucket + composed key in the path.
        assert!(url.starts_with("http://minio.local:9000"), "url: {url}");
        assert!(url.contains("lab-data"), "bucket in url: {url}");
        assert!(url.contains("datasets/runs/a.h5"), "composed key in url: {url}");
        // AWS SigV4 query params present → it's actually signed.
        assert!(
            url.contains("X-Amz-Signature") && url.contains("X-Amz-Expires"),
            "signed query: {url}"
        );
    }

    /// The default (non-proxy) external-s3 branch returns a 302 redirect to the
    /// presigned URL; the proxy branch streams 200/206. We drive only the
    /// presign half here (no live store) but assert the redirect shape.
    #[tokio::test]
    async fn presign_redirect_branch_is_302_with_location() {
        use axum::response::IntoResponse;
        let t = s3_target("");
        let url = presign_s3(&t, "a.txt", Duration::from_secs(60))
            .await
            .unwrap();
        // Reconstruct the same response the default serve branch builds.
        let resp = match header::HeaderValue::from_str(&url) {
            Ok(loc) => (StatusCode::FOUND, [(header::LOCATION, loc)]).into_response(),
            Err(_) => unreachable!("minted URL is a valid header value"),
        };
        assert_eq!(resp.status(), StatusCode::FOUND);
        assert!(resp.headers().contains_key(header::LOCATION));
    }
}
