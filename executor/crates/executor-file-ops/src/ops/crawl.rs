//! Crawl operation — recursive, streaming, checkpointable directory walk.
//!
//! `crawl` is `list`'s recursive, streaming sibling, built for the
//! legacy-migration's ~4M-file corpus (docs/32). Unlike [`list`](super::list),
//! which buffers the entire `Vec<Entry>` and returns it inline, `crawl`:
//!
//! - drives the OpenDAL **streaming** lister (`lister_with(..).recursive(true)`)
//!   as a `futures::Stream`, so the whole listing never lands in memory at once;
//! - `stat()`s each FILE entry for `{size, mtime}` (the `fs` lister returns
//!   entries WITHOUT `content_length`/`last_modified`, so this is mandatory to
//!   capture size+mtime — mirrors [`list`](super::list) when `include_stat`);
//! - emits fixed-size batches — either over the job's
//!   [`EventStream`](aithericon_executor_backend::traits::EventStream)
//!   `item()`/`close()` channel mechanism (docs/25 consumer-join), or, in
//!   **sink mode** (`config.sink`), as durable [`FoldBatch`] publishes to the
//!   injected [`BatchSink`] so the control plane folds them set-based into
//!   `file_inventory` without any per-file engine tokens;
//! - is **cancellable between batches**, **resumable** via `resume_from`
//!   (`start_after`), and **chunkable** via `max_batches`, so a 4M-file walk
//!   runs as a cursor-loop campaign.
//!
//! It performs **no `read`** — metadata only. Integrity-hashing remains the
//! `probe` op's job, run later against only the orphans/mismatches.

use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt;
use opendal::Operator;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use uuid::Uuid;

use aithericon_executor_backend::traits::{BatchSink, EventStream};
use aithericon_executor_domain::{FoldBatch, FoldItem, FoldMode};

use crate::config::CrawlConfig;

use super::{resolve_path, FileOpsError, FileOpsResult};

/// Channel name the crawl batches are emitted on (events mode).
const CRAWL_CHANNEL: &str = "crawl";

/// One accumulated file observation, mode-agnostic until emit time.
struct Observed {
    path: String,
    size: u64,
    mtime: Option<String>,
}

/// Where filled batches go — resolved once from config + injected sink.
enum Emitter {
    /// Legacy/default: `item()`/`close()` on the job's EventStream channel
    /// (no-op when the job didn't opt into streaming).
    Events(Option<Arc<dyn EventStream>>),
    /// Sink mode: durable `FoldBatch` publishes; the resume cursor advances
    /// strictly AFTER a successful publish (a failure errors the job and a
    /// retry replays from the last durable batch — fold upserts are
    /// idempotent on `(file_server_id, path)`).
    Sink {
        sink: Arc<dyn BatchSink>,
        mode: FoldMode,
        file_server_id: String,
        execution_id: String,
        endpoint_root: String,
    },
}

impl Emitter {
    async fn emit(
        &self,
        episode_uid: &str,
        idx: u64,
        items: Vec<Observed>,
        endpoint_root: &str,
    ) -> Result<(), FileOpsError> {
        match self {
            Emitter::Events(stream) => {
                if let Some(es) = stream {
                    let items: Vec<serde_json::Value> = items
                        .into_iter()
                        .map(|o| {
                            serde_json::json!({
                                "path": o.path,
                                "size": o.size,
                                "mtime": o.mtime,
                                "endpoint_root": endpoint_root,
                            })
                        })
                        .collect();
                    es.item(
                        CRAWL_CHANNEL.to_string(),
                        episode_uid.to_string(),
                        idx,
                        serde_json::json!({ "items": items }),
                    )
                    .await;
                }
                Ok(())
            }
            Emitter::Sink {
                sink,
                mode,
                file_server_id,
                execution_id,
                endpoint_root,
            } => {
                let batch = FoldBatch {
                    execution_id: execution_id.clone(),
                    episode_uid: episode_uid.to_string(),
                    batch_idx: idx,
                    mode: *mode,
                    file_server_id: file_server_id.clone(),
                    endpoint_root: endpoint_root.clone(),
                    // Stamped by the NATS sink (runner identity lives there).
                    serve_group: None,
                    items: items
                        .into_iter()
                        .map(|o| FoldItem {
                            path: o.path,
                            size: o.size,
                            mtime: o.mtime,
                            hash: None,
                        })
                        .collect(),
                };
                sink.publish(&batch)
                    .await
                    .map_err(|e| FileOpsError::Config(format!("crawl: batch sink publish: {e}")))
            }
        }
    }
}

/// Recursively walk `config.prefix`, streaming `{path,size,mtime}` batches.
///
/// # Batch routing
///
/// Default (no `config.sink`): one `item(CRAWL_CHANNEL, episode_uid, idx,
/// {"items":[…]})` per filled batch + a single `close(CRAWL_CHANNEL,
/// episode_uid, batch_idx)` (the close count is the number of batches — what a
/// `gather` barrier sizes itself on, NOT the file `total`). When
/// `event_stream` is `None`, batches are not emitted but the walk still
/// completes and reports `count`/`last_path`.
///
/// Sink mode (`config.sink` set): each filled batch (and the trailing partial)
/// is published durably through `batch_sink`; NO channel items/closes are
/// emitted. A publish failure is a hard error.
///
/// # Outputs
///
/// - `prefix` — the prefix from the config
/// - `count` — total number of FILE entries crawled this invocation
/// - `last_path` — the user-facing path of the last file emitted (the resume
///   cursor for the next chunk), or `null` if nothing was crawled
/// - `batches` — number of batches emitted/published
/// - `cancelled` — the walk stopped on cancellation
/// - `exhausted` — the lister reached EOF (not stopped by cancellation or
///   `max_batches`); the cursor-loop campaign's exit condition
/// - `endpoint_root` — the canonical (server-relative) root the emitted
///   `path`s are anchored to. Recorded so the registered
///   `file_inventory.path` stays canonical and an `adopt` can stamp it onto
///   the file-server endpoint root.
///
/// Events-mode batch items carry `endpoint_root` per item; sink-mode batches
/// carry it once on the [`FoldBatch`] envelope.
#[allow(clippy::too_many_arguments)]
pub async fn execute(
    config: &CrawlConfig,
    operator: &Operator,
    prefix: &str,
    endpoint_root: &str,
    event_stream: Option<Arc<dyn EventStream>>,
    batch_sink: Option<Arc<dyn BatchSink>>,
    execution_id: &str,
    cancel: &CancellationToken,
) -> FileOpsResult {
    let full_prefix = resolve_path(prefix, &config.prefix);

    // Resolve the emitter once. Sink mode requires a host-injected sink —
    // failing here (not silently degrading to events) keeps the "batches are
    // durable" contract honest.
    let emitter = match &config.sink {
        Some(sc) => {
            let sink = batch_sink.ok_or_else(|| {
                FileOpsError::Config(
                    "crawl: sink mode requested but the executor has no batch sink configured"
                        .into(),
                )
            })?;
            let mode = match sc.mode.as_str() {
                "reconcile" => FoldMode::Reconcile,
                "index" => FoldMode::Index,
                other => {
                    return Err(FileOpsError::Config(format!(
                        "crawl: sink.mode must be 'reconcile' or 'index', got '{other}'"
                    )))
                }
            };
            Emitter::Sink {
                sink,
                mode,
                file_server_id: sc.file_server_id.clone(),
                execution_id: execution_id.to_string(),
                endpoint_root: endpoint_root.to_string(),
            }
        }
        None => Emitter::Events(event_stream.clone()),
    };

    // Build the recursive streaming lister. `resume_from` is supplied as a
    // user-facing path; re-apply the storage prefix so `start_after` matches
    // the lister's own (prefixed) entry paths. An EMPTY string counts as
    // absent — interpolated campaign configs deliver `""` on iteration 0.
    //
    // Resume strategy is capability-aware: only S3-style backends implement
    // `start_after` natively (the `fs` lister SILENTLY IGNORES it — a resumed
    // chunk would re-walk from the start and a max_batches campaign would
    // re-emit the same first chunk forever). Without native support we walk
    // from the start and SKIP entries until we pass the exact cursor path —
    // the skip happens before the per-entry `stat()`, so a resumed re-walk
    // costs readdir only. This assumes the backend enumerates an unchanged
    // tree in a stable order (true for `fs`); a vanished cursor is a hard
    // error rather than a silent restart.
    let resume = config.resume_from.as_deref().filter(|s| !s.is_empty());
    let native_start_after = operator.info().full_capability().list_with_start_after;
    let mut lister_fut = operator.lister_with(&full_prefix).recursive(true);
    if let (Some(resume), true) = (resume, native_start_after) {
        let full_resume = resolve_path(prefix, resume);
        lister_fut = lister_fut.start_after(&full_resume);
    }
    let mut lister = lister_fut.await?;
    // Client-side skip phase active until the cursor is passed.
    let mut resume_passed = resume.is_none() || native_start_after;

    // One episode for the whole walk: items + close share this uid.
    let episode_uid = Uuid::new_v4().to_string();

    let batch_cap = config.batch_size.max(1);
    let mut batch: Vec<Observed> = Vec::with_capacity(batch_cap);
    let mut total: u64 = 0;
    let mut batch_idx: u64 = 0;
    let mut last_path: Option<String> = None;
    let mut cancelled = false;
    let mut stopped_by_max = false;

    while let Some(entry) = lister.next().await {
        let entry = entry?;
        let path = entry.path().to_string();

        // Skip directory markers — both the trailing-slash convention and the
        // entry's own mode (the `fs` lister marks directories via mode).
        if path.ends_with('/') || entry.metadata().is_dir() {
            continue;
        }

        // Strip the storage prefix back to user-facing paths.
        let user_path = if !prefix.is_empty() {
            path.strip_prefix(prefix).unwrap_or(&path).to_string()
        } else {
            path.clone()
        };

        // Client-side resume: skip up to AND INCLUDING the cursor path.
        // Placed before the stat() so the skipped span costs readdir only.
        if !resume_passed {
            if Some(user_path.as_str()) == resume {
                resume_passed = true;
            }
            continue;
        }

        // The `fs` lister returns entries without size/mtime, so stat each file
        // when requested (the default). `path` here is the full storage path.
        let (size, mtime) = if config.stat {
            let meta = operator.stat(&path).await?;
            // `last_modified()` is an opendal `Timestamp` (RFC 3339 via Display),
            // mirroring `list`'s `include_stat` rendering.
            let mtime = meta.last_modified().map(|t| t.to_string());
            (meta.content_length(), mtime)
        } else {
            (entry.metadata().content_length(), None)
        };

        batch.push(Observed {
            path: user_path.clone(),
            size,
            mtime,
        });
        total += 1;
        last_path = Some(user_path);

        if batch.len() >= batch_cap {
            emitter
                .emit(
                    &episode_uid,
                    batch_idx,
                    std::mem::take(&mut batch),
                    endpoint_root,
                )
                .await?;
            batch_idx += 1;

            // Chunk cap for cursor-loop campaigns: stop after N filled
            // batches; the caller resumes from `last_path`.
            if let Some(max) = config.max_batches {
                if batch_idx >= max {
                    stopped_by_max = true;
                    break;
                }
            }

            // Honor cancellation between batches — stop gracefully, reporting
            // what we crawled so far (the caller resumes from `last_path`).
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
        }
    }

    // A client-side resume that never found its cursor means the tree changed
    // since the last chunk (cursor file deleted/renamed). Erroring is honest;
    // silently restarting could re-emit the same chunk forever.
    if !resume_passed {
        return Err(FileOpsError::Config(format!(
            "crawl: resume_from '{}' not found in listing — the tree changed \
             since the last chunk; restart the campaign without resume_from",
            resume.unwrap_or_default()
        )));
    }

    // Flush any partial trailing batch (only on natural EOF; on cancel or
    // max_batches we already broke right after emitting a full batch).
    if !cancelled && !stopped_by_max && !batch.is_empty() {
        emitter
            .emit(
                &episode_uid,
                batch_idx,
                std::mem::take(&mut batch),
                endpoint_root,
            )
            .await?;
        batch_idx += 1;
    }

    // Close the episode so a downstream gather barrier knows the final count
    // (events mode only — sink mode emits no channel tokens at all).
    // The count MUST be the number of `item()` calls emitted (one per batch =
    // `batch_idx`), NOT the file `total` — a `join: gather` consumer sizes its
    // barrier on the number of items, so passing the file count would make it
    // wait for files-many items and hang (it only ever receives `batch_idx`).
    if let Emitter::Events(Some(ref es)) = emitter {
        es.close(CRAWL_CHANNEL.to_string(), episode_uid.clone(), batch_idx)
            .await;
    }

    let exhausted = !cancelled && !stopped_by_max;

    debug!(
        prefix = %config.prefix,
        total,
        batches = batch_idx,
        cancelled,
        exhausted,
        "crawl complete"
    );

    Ok(HashMap::from([
        ("prefix".into(), serde_json::json!(config.prefix)),
        ("count".into(), serde_json::json!(total)),
        ("last_path".into(), serde_json::json!(last_path)),
        ("batches".into(), serde_json::json!(batch_idx)),
        ("cancelled".into(), serde_json::json!(cancelled)),
        ("exhausted".into(), serde_json::json!(exhausted)),
        ("endpoint_root".into(), serde_json::json!(endpoint_root)),
    ]))
}
