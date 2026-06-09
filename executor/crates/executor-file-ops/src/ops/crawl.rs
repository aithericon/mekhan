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
//! - emits fixed-size batches over the job's
//!   [`EventStream`](aithericon_executor_backend::traits::EventStream)
//!   `item()`/`close()` channel mechanism (docs/25 consumer-join), so a
//!   downstream consumer folds the stream into `file_inventory` rows;
//! - is **cancellable between batches** and **resumable** via `resume_from`
//!   (`start_after`), so a 4M-file walk can be checkpointed.
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

use aithericon_executor_backend::traits::EventStream;

use crate::config::CrawlConfig;

use super::{resolve_path, FileOpsResult};

/// Channel name the crawl batches are emitted on.
const CRAWL_CHANNEL: &str = "crawl";

/// Recursively walk `config.prefix`, streaming `{path,size,mtime}` batches.
///
/// # Events
///
/// When `event_stream` is `Some`, one `item(CRAWL_CHANNEL, episode_uid, idx,
/// {"items":[{path,size,mtime}, …]})` is emitted per batch of `config.batch_size`
/// files, followed by a single `close(CRAWL_CHANNEL, episode_uid, batch_idx)` at
/// the end (the close count is the number of items/batches emitted, which is
/// what a `gather` barrier sizes itself on — NOT the file `total`). All items +
/// the close share one `episode_uid`. When `event_stream` is `None` (the job
/// didn't opt into streaming), batches are not emitted — the operation still
/// runs to completion and reports `count`/`last_path` in its outputs, which is
/// what the direct-call test path and small crawls rely on.
///
/// # Outputs
///
/// - `prefix` — the prefix from the config
/// - `count` — total number of FILE entries crawled
/// - `last_path` — the user-facing path of the last file emitted (the resume
///   cursor for the next chunk), or `null` if nothing was crawled
/// - `batches` — number of `item` batches emitted
/// - `endpoint_root` — the canonical (server-relative) root the emitted `path`s
///   are anchored to. Recorded so the registered `file_inventory.path` stays
///   canonical and an `adopt` can stamp it onto the file-server endpoint root.
///
/// Each per-file batch item also carries `endpoint_root`, so the downstream
/// fold/register can persist it into the inventory row's `provenance` JSONB.
pub async fn execute(
    config: &CrawlConfig,
    operator: &Operator,
    prefix: &str,
    endpoint_root: &str,
    event_stream: Option<Arc<dyn EventStream>>,
    cancel: &CancellationToken,
) -> FileOpsResult {
    let full_prefix = resolve_path(prefix, &config.prefix);

    // Build the recursive streaming lister. `resume_from` is supplied as a
    // user-facing path; re-apply the storage prefix so `start_after` matches
    // the lister's own (prefixed) entry paths.
    let mut lister_fut = operator.lister_with(&full_prefix).recursive(true);
    if let Some(ref resume) = config.resume_from {
        let full_resume = resolve_path(prefix, resume);
        lister_fut = lister_fut.start_after(&full_resume);
    }
    let mut lister = lister_fut.await?;

    // One episode for the whole walk: items + close share this uid.
    let episode_uid = Uuid::new_v4().to_string();

    let mut batch: Vec<serde_json::Value> = Vec::with_capacity(config.batch_size.max(1));
    let mut total: u64 = 0;
    let mut batch_idx: u64 = 0;
    let mut last_path: Option<String> = None;
    let mut cancelled = false;

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

        batch.push(serde_json::json!({
            "path": user_path,
            "size": size,
            "mtime": mtime,
            "endpoint_root": endpoint_root,
        }));
        total += 1;
        last_path = Some(user_path);

        if batch.len() >= config.batch_size.max(1) {
            emit_batch(&event_stream, &episode_uid, batch_idx, std::mem::take(&mut batch)).await;
            batch_idx += 1;

            // Honor cancellation between batches — stop gracefully, reporting
            // what we crawled so far (the caller resumes from `last_path`).
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
        }
    }

    // Flush any partial trailing batch (only when not cancelled mid-stream;
    // on cancel we already broke after emitting the full batch).
    if !cancelled && !batch.is_empty() {
        emit_batch(&event_stream, &episode_uid, batch_idx, std::mem::take(&mut batch)).await;
        batch_idx += 1;
    }

    // Close the episode so a downstream gather barrier knows the final count.
    // The count MUST be the number of `item()` calls emitted (one per batch =
    // `batch_idx`), NOT the file `total` — a `join: gather` consumer sizes its
    // barrier on the number of items, so passing the file count would make it
    // wait for files-many items and hang (it only ever receives `batch_idx`).
    if let Some(ref es) = event_stream {
        es.close(CRAWL_CHANNEL.to_string(), episode_uid.clone(), batch_idx)
            .await;
    }

    debug!(
        prefix = %config.prefix,
        total,
        batches = batch_idx,
        cancelled,
        "crawl complete"
    );

    Ok(HashMap::from([
        ("prefix".into(), serde_json::json!(config.prefix)),
        ("count".into(), serde_json::json!(total)),
        ("last_path".into(), serde_json::json!(last_path)),
        ("batches".into(), serde_json::json!(batch_idx)),
        ("cancelled".into(), serde_json::json!(cancelled)),
        ("endpoint_root".into(), serde_json::json!(endpoint_root)),
    ]))
}

/// Emit one batch as an `item` event (no-op when streaming is disabled).
async fn emit_batch(
    event_stream: &Option<Arc<dyn EventStream>>,
    episode_uid: &str,
    idx: u64,
    items: Vec<serde_json::Value>,
) {
    if let Some(es) = event_stream {
        es.item(
            CRAWL_CHANNEL.to_string(),
            episode_uid.to_string(),
            idx,
            serde_json::json!({ "items": items }),
        )
        .await;
    }
}
