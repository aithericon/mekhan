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
//!   on a LOCAL backend the stat is a direct `lstat` of
//!   `<endpoint>/<storage path>`, additionally capturing `{uid, gid, mode}`
//!   (ownership facts an opendal `Metadata` cannot carry);
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
//! By default it performs **no `read`** — metadata only, with
//! integrity-hashing left to the `probe` op run later against only the
//! orphans/mismatches. `config.probe` opts into per-entry content probing
//! during the walk itself (`"hash"` = SHA-256 only, `"full"` = SHA-256 +
//! `fmeta` metadata) for corpora that want content identity captured on the
//! first pass; probe failures are counted (`probe_errors`), never fatal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use futures::stream::FuturesOrdered;
use futures::StreamExt;
use opendal::Operator;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use aithericon_executor_backend::traits::{BatchSink, EventStream};
use aithericon_executor_domain::{
    FoldBatch, FoldItem, FoldMode, LogLevel, MetricPoint, MetricType,
};

/// How often the background sampler emits a throughput sample — both a
/// `MetricPoint` batch (when the worker has a metric sink) and an `info!` log
/// line (always). A long walk is otherwise silent; this gives live throughput
/// without spamming.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(2);

/// Live crawl progress, shared between the walk loop and the background sampler
/// task. The sampler emits on its OWN timer ([`PROGRESS_INTERVAL`]) so samples
/// keep flowing even while the walk loop is blocked for minutes inside a single
/// large-file probe — the case the plain per-file emit could never cover (a
/// 43 GB probe would otherwise look stalled, `files_per_second` pinned at 0).
#[derive(Default)]
struct CrawlProgress {
    /// Cumulative FILE entries observed so far.
    files: AtomicU64,
    /// Cumulative bytes ingested. On Linux this is driven by `/proc/self/io`
    /// `rchar` (see [`proc_rchar`]) so it advances MID-FILE — a 43 GB probe
    /// reports throughput while it reads, not a flat 0 then a spike at
    /// completion. On other platforms the walk loop adds each probed file's
    /// size after the probe returns (file-granular fallback).
    bytes: AtomicU64,
}

/// Bytes this process has read via `read(2)`-family syscalls so far, from
/// `/proc/self/io` `rchar`. This is the read-path-agnostic way to see ingest
/// progress MID-FILE: whichever code actually reads the bytes (the checksum
/// loop, fmeta, opendal) bumps the kernel counter, so the sampler sees a 43 GB
/// probe advancing in real time without threading a progress callback through
/// every backend. Process-wide (includes the odd NATS/log read), but on a crawl
/// runner those are negligible against multi-MB/s file reads. `None` off Linux.
#[cfg(target_os = "linux")]
fn proc_rchar() -> Option<u64> {
    let io = std::fs::read_to_string("/proc/self/io").ok()?;
    io.lines()
        .find_map(|l| l.strip_prefix("rchar:"))
        .and_then(|v| v.trim().parse().ok())
}

#[cfg(not(target_os = "linux"))]
fn proc_rchar() -> Option<u64> {
    None
}

/// Aborts the background sampler when the walk returns — on the happy path AND
/// on any early `?`/error/panic — so a finished crawl never leaves a task
/// emitting metrics against a dead execution.
struct SamplerGuard(JoinHandle<()>);
impl Drop for SamplerGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Spawn the periodic sampler. It reads the shared [`CrawlProgress`] every
/// [`PROGRESS_INTERVAL`] and, on Linux, refreshes `bytes` from `rchar` so the
/// rate reflects mid-file progress. Runs on its own task: the multi-threaded
/// worker runtime keeps ticking it while the walk task blocks in a probe.
fn spawn_crawl_sampler(
    progress: Arc<CrawlProgress>,
    event_stream: Option<Arc<dyn EventStream>>,
    file_server: Option<String>,
    rchar_baseline: Option<u64>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
        ticker.tick().await; // consume the immediate first tick
        let mut last = Instant::now();
        let mut last_files = 0u64;
        let mut last_bytes = 0u64;
        loop {
            ticker.tick().await;
            // On Linux, derive cumulative bytes from rchar so the gauge climbs
            // during a long single-file read; elsewhere the walk loop owns it.
            if let (Some(base), Some(now)) = (rchar_baseline, proc_rchar()) {
                progress
                    .bytes
                    .store(now.saturating_sub(base), Ordering::Relaxed);
            }
            let files = progress.files.load(Ordering::Relaxed);
            let bytes = progress.bytes.load(Ordering::Relaxed);
            if files == last_files && bytes == last_bytes {
                continue; // nothing moved — stay quiet, keep accumulating the window
            }
            let window = last.elapsed();
            emit_crawl_progress(
                &event_stream,
                file_server.as_deref(),
                files,
                files - last_files,
                bytes,
                bytes - last_bytes,
                window,
            )
            .await;
            last = Instant::now();
            last_files = files;
            last_bytes = bytes;
        }
    })
}

/// Emit one crawl progress sample: `crawl/files_per_second` +
/// `crawl/bytes_per_second` (gauges over the window since the last sample) plus
/// `crawl/files_total` + `crawl/bytes_total` (cumulative gauges), and an
/// always-on `info!` line. `bytes_per_second` is the liveness signal that keeps
/// moving through a single huge probe where `files_per_second` would read 0.
/// Metrics ride [`EventStream::metric`] → the same sink child-process SDK
/// metrics use, so they surface wherever `train/loss` does. A no-op metric sink
/// still leaves the log line.
#[allow(clippy::too_many_arguments)]
async fn emit_crawl_progress(
    event_stream: &Option<Arc<dyn EventStream>>,
    file_server: Option<&str>,
    total: u64,
    window_files: u64,
    bytes_total: u64,
    window_bytes: u64,
    window: Duration,
) {
    let secs = window.as_secs_f64();
    let (files_per_sec, bytes_per_sec) = if secs > 0.0 {
        (window_files as f64 / secs, window_bytes as f64 / secs)
    } else {
        (0.0, 0.0)
    };
    info!(
        total,
        files_per_sec = format!("{files_per_sec:.1}"),
        mib_per_sec = format!("{:.1}", bytes_per_sec / (1024.0 * 1024.0)),
        "crawl: progress"
    );
    let Some(es) = event_stream else { return };
    let mut labels = HashMap::new();
    labels.insert("op".to_string(), "crawl".to_string());
    if let Some(fs) = file_server {
        labels.insert("file_server".to_string(), fs.to_string());
    }
    let ts = Utc::now();
    let gauge = |name: &str, value: f64| MetricPoint {
        name: name.to_string(),
        value,
        step: None,
        timestamp: ts,
        metric_type: MetricType::Gauge,
        labels: labels.clone(),
    };
    es.metric(vec![
        gauge("crawl/files_per_second", files_per_sec),
        gauge("crawl/files_total", total as f64),
        gauge("crawl/bytes_per_second", bytes_per_sec),
        gauge("crawl/bytes_total", bytes_total as f64),
    ])
    .await;
}

/// Stream a diagnostic line to the run's log panel (`executor.events.{exec_id}.log`)
/// when an `EventStream` is wired. Crawl traversal/probe errors otherwise reach
/// only the runner's LOCAL `warn!` tracing, where a user never sees them — so a
/// permission-walled crawl looks like it "finished with N files" and silently
/// drops the rest. No-op without a stream (sink-less unit runs).
async fn crawl_log(event_stream: &Option<Arc<dyn EventStream>>, level: LogLevel, message: String) {
    if let Some(es) = event_stream {
        es.log(level, message, HashMap::new()).await;
    }
}

/// Per-class cap on individually-streamed error lines (list errors and probe
/// errors each get their own budget). A corpus with thousands of unreadable
/// entries must not flood the run log — beyond the cap the errors are still
/// counted and reported in the completion summary, and `warn!` tracing stays
/// uncapped on the runner host.
const MAX_STREAMED_DIAGNOSTICS: u64 = 25;

use crate::config::CrawlConfig;

use super::{local_stat, local_stat_root, resolve_path, FileOpsError, FileOpsResult};

/// Channel name the crawl batches are emitted on (events mode).
const CRAWL_CHANNEL: &str = "crawl";

/// One accumulated file observation, mode-agnostic until emit time.
struct Observed {
    path: String,
    size: u64,
    mtime: Option<String>,
    /// `st_uid`/`st_gid`/`st_mode` — only populated when the storage backend
    /// is local (a direct lstat); opendal metadata cannot carry ownership.
    uid: Option<u32>,
    gid: Option<u32>,
    mode: Option<u32>,
    /// SHA-256 (bare lowercase hex), when `config.probe` is on and the file
    /// probed cleanly.
    hash: Option<String>,
    /// Full `fmeta` blob, when `config.probe == "full"`.
    metadata: Option<serde_json::Value>,
}

/// Per-entry content probing level, parsed once from `config.probe`.
#[derive(Clone, Copy, PartialEq)]
enum ProbeMode {
    Off,
    Hash,
    Full,
}

impl ProbeMode {
    /// `""`/`"off"`/`"none"` all mean Off so a select-field or interpolated
    /// start parameter can express the disabled state.
    fn parse(s: Option<&str>) -> Result<Self, String> {
        match s.map(str::trim) {
            None | Some("" | "off" | "none") => Ok(ProbeMode::Off),
            Some("hash") => Ok(ProbeMode::Hash),
            Some("full") => Ok(ProbeMode::Full),
            Some(other) => Err(format!(
                "crawl: probe must be 'hash', 'full', or 'off'/empty, got '{other}'"
            )),
        }
    }
}

/// Probe one crawled entry for content identity (and, in `Full` mode, fmeta
/// metadata). On a LOCAL backend the file is probed in place at
/// `<endpoint>/<storage path>` (zero copy — the co-located-runner hot path);
/// otherwise it is downloaded to a temp file under `run_dir` first, mirroring
/// the standalone `probe` op. Returns `(hash, metadata)`; the error string is
/// caller-counted, never fatal to the walk.
async fn probe_entry(
    mode: ProbeMode,
    lstat_root: Option<&Path>,
    operator: &Operator,
    storage_path: &str,
    run_dir: &Path,
) -> Result<(Option<String>, Option<serde_json::Value>), String> {
    let (local_path, tmp): (PathBuf, Option<PathBuf>) = match lstat_root {
        Some(root) => (root.join(storage_path), None),
        None => {
            let data = operator
                .read(storage_path)
                .await
                .map_err(|e| format!("read: {e}"))?;
            // Preserve the extension for fmeta format detection.
            let ext = Path::new(storage_path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("bin");
            let tmp_path = run_dir.join(format!("_crawl_probe_tmp.{ext}"));
            if let Some(parent) = tmp_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("tmp dir: {e}"))?;
            }
            tokio::fs::write(&tmp_path, data.to_vec())
                .await
                .map_err(|e| format!("tmp write: {e}"))?;
            (tmp_path.clone(), Some(tmp_path))
        }
    };

    let result = match mode {
        ProbeMode::Off => Ok((None, None)),
        ProbeMode::Hash => {
            let p = local_path.clone();
            tokio::task::spawn_blocking(move || {
                aithericon_file_metadata::compute_checksum(
                    &p,
                    aithericon_file_metadata::ChecksumAlgorithm::Sha256,
                )
            })
            .await
            .map_err(|e| format!("checksum task: {e}"))?
            .map_err(|e| format!("checksum: {e}"))
            .map(|info| (Some(info.digest), None))
        }
        ProbeMode::Full => {
            // SHA-256 always; unsupported formats degrade to checksum-only
            // inside extract_metadata_async (the fmeta-side fallback).
            aithericon_file_metadata::extract_metadata_async(&local_path)
                .await
                .map_err(|e| format!("fmeta: {e}"))
                .and_then(|meta| {
                    let hash = meta.checksum.as_ref().map(|c| c.digest.clone());
                    let blob =
                        serde_json::to_value(&meta).map_err(|e| format!("serialize: {e}"))?;
                    Ok((hash, Some(blob)))
                })
        }
    };

    if let Some(tmp) = tmp {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    result
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
                                "uid": o.uid,
                                "gid": o.gid,
                                "mode": o.mode,
                                "hash": o.hash,
                                "metadata": o.metadata,
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
                            hash: o.hash,
                            uid: o.uid,
                            gid: o.gid,
                            mode: o.mode,
                            metadata: o.metadata,
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
/// - `probe_errors` — number of entries whose `config.probe` read/hash failed
///   (emitted hashless instead of failing the walk); `0` when probing is off.
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
    run_dir: &Path,
    cancel: &CancellationToken,
) -> FileOpsResult {
    let full_prefix = resolve_path(prefix, &config.prefix);
    let probe_mode = ProbeMode::parse(config.probe.as_deref()).map_err(FileOpsError::Config)?;

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

    // Local backend → lstat directly at `<endpoint>/<storage path>` (the Fs
    // operator's root IS the endpoint): size + mtime + uid/gid/mode in ONE
    // syscall, where the opendal stat would cost the same syscall yet drop
    // ownership. Non-local (or any lstat error) falls back to opendal below.
    let lstat_root = local_stat_root(&config.storage);

    let batch_cap = config
        .batch_size
        .get("crawl: batch_size")
        .map_err(FileOpsError::Config)?
        .max(1);
    let max_batches = config
        .max_batches
        .as_ref()
        .map(|m| m.get("crawl: max_batches"))
        .transpose()
        .map_err(FileOpsError::Config)?;
    let mut batch: Vec<Observed> = Vec::with_capacity(batch_cap);
    let mut total: u64 = 0;
    let mut batch_idx: u64 = 0;
    let mut last_path: Option<String> = None;
    let mut cancelled = false;
    let mut stopped_by_max = false;
    let mut probe_errors: u64 = 0;
    // Directory `list` failures (EACCES on a restricted subtree, or a dir that
    // vanished mid-walk) are tolerated like probe errors: a large corpus WILL
    // contain unreadable dirs, and aborting the whole campaign on one is worse
    // than skipping it. opendal's `FlatLister` drops the failed subdir and
    // resumes the parent on the next poll, so we log, count, and continue.
    // `consecutive_list_errors` guards the rarer mid-read case where the SAME
    // lister keeps erroring — bail rather than spin forever.
    let mut list_errors: u64 = 0;
    let mut consecutive_list_errors: u32 = 0;
    const MAX_CONSECUTIVE_LIST_ERRORS: u32 = 100;

    // Live throughput sampling — see `spawn_crawl_sampler`/`emit_crawl_progress`.
    // `file_server` labels the metric in sink mode (the only mode with a server
    // identity). The sampler runs on its own task so samples keep flowing even
    // while this loop is blocked for minutes inside one large-file probe.
    let file_server = config.sink.as_ref().map(|sc| sc.file_server_id.clone());
    let crawl_start = Instant::now();
    let progress = Arc::new(CrawlProgress::default());
    // Capture the rchar baseline BEFORE any probe reads; if present (Linux) the
    // sampler owns `bytes` from rchar and the loop must not also add to it.
    let rchar_baseline = proc_rchar();
    let rchar_active = rchar_baseline.is_some();
    let _sampler = SamplerGuard(spawn_crawl_sampler(
        progress.clone(),
        event_stream.clone(),
        file_server.clone(),
        rchar_baseline,
    ));

    // Per-file content probing runs up to `probe_concurrency` in flight. The walk
    // still LISTS + `stat`s sequentially (cheap), but the expensive read + SHA-256
    // + `fmeta` parse — frequently latency-bound (many small random reads on a
    // RAID array, or one multi-GB file) — overlaps across files. `FuturesOrdered`
    // yields results in LISTING ORDER, so batches and the `last_path` resume
    // cursor stay byte-identical to a sequential walk; only wall-clock changes.
    // `1` restores the historical one-at-a-time behavior.
    let concurrency = config
        .probe_concurrency
        .get("crawl: probe_concurrency")
        .map_err(FileOpsError::Config)?
        .max(1);

    // Stat fields carried alongside each in-flight probe, so a drained result can
    // be turned into an `Observed` in listing order.
    struct Pending {
        user_path: String,
        size: u64,
        mtime: Option<String>,
        uid: Option<u32>,
        gid: Option<u32>,
        mode: Option<u32>,
    }

    let mut inflight = FuturesOrdered::new();
    let mut lister_done = false;

    'walk: loop {
        // Fill the in-flight window: pull entries (skipping dir markers / the
        // resumed span / unreadable subtrees), `stat` them, and enqueue one probe
        // each until `concurrency` are running or the lister is drained. Stop
        // topping up the instant a cancel arrives so we don't queue work we're
        // about to abandon.
        while !lister_done && inflight.len() < concurrency && !cancel.is_cancelled() {
            let entry = match lister.next().await {
                None => {
                    lister_done = true;
                    break;
                }
                Some(Ok(e)) => {
                    consecutive_list_errors = 0;
                    e
                }
                // Skip unreadable / vanished directories instead of failing the
                // whole walk. opendal's `FlatLister` has already dropped the
                // failed subdir, so `continue` resumes the parent's remaining
                // entries; the error message carries the offending path.
                Some(Err(e))
                    if matches!(
                        e.kind(),
                        opendal::ErrorKind::PermissionDenied | opendal::ErrorKind::NotFound
                    ) =>
                {
                    list_errors += 1;
                    consecutive_list_errors += 1;
                    if consecutive_list_errors > MAX_CONSECUTIVE_LIST_ERRORS {
                        warn!(
                            error = %e,
                            consecutive = consecutive_list_errors,
                            "crawl: too many consecutive list errors; aborting walk"
                        );
                        return Err(e.into());
                    }
                    warn!(error = %e, "crawl: list error; skipping unreadable path");
                    if list_errors <= MAX_STREAMED_DIAGNOSTICS {
                        crawl_log(
                            &event_stream,
                            LogLevel::Warn,
                            format!("skipped unreadable directory ({}): {e}", e.kind()),
                        )
                        .await;
                    }
                    continue;
                }
                // Genuine infrastructure errors still abort the walk.
                Some(Err(e)) => return Err(e.into()),
            };
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

            // The `fs` lister returns entries without size/mtime, so stat each
            // file when requested (the default). `path` is the full storage path.
            let (size, mtime, uid, gid, mode) = if config.stat {
                match lstat_root
                    .as_deref()
                    .and_then(|root| local_stat(root, &path))
                {
                    Some(s) => (s.size, s.mtime, s.uid, s.gid, s.mode),
                    None => {
                        let meta = operator.stat(&path).await?;
                        // `last_modified()` is an opendal `Timestamp` (RFC 3339 via
                        // Display), mirroring `list`'s `include_stat` rendering.
                        let mtime = meta.last_modified().map(|t| t.to_string());
                        (meta.content_length(), mtime, None, None, None)
                    }
                }
            } else {
                (entry.metadata().content_length(), None, None, None, None)
            };

            // Enqueue the probe. It OWNS its inputs (cheap clones — `Operator` is
            // Arc-backed) so it's self-contained and runs concurrently with its
            // siblings and the listing loop. `Off` mode short-circuits to a
            // hashless result. Probe failures are RETURNED (not raised) and
            // counted at the drain site, where the shared counters live.
            let pending = Pending {
                user_path,
                size,
                mtime,
                uid,
                gid,
                mode,
            };
            let op = operator.clone();
            let rdir = run_dir.to_path_buf();
            let lroot = lstat_root.clone();
            let storage_path = path;
            let mode_for_probe = probe_mode;
            inflight.push_back(async move {
                let res = if mode_for_probe == ProbeMode::Off {
                    Ok((None, None))
                } else {
                    probe_entry(mode_for_probe, lroot.as_deref(), &op, &storage_path, &rdir).await
                };
                (pending, res)
            });
        }

        // Lister drained and nothing in flight → the walk is complete.
        if inflight.is_empty() {
            if cancel.is_cancelled() {
                cancelled = true;
            }
            break 'walk;
        }

        // Take the OLDEST completed probe (listing order). A cancel interrupts the
        // wait so a single in-flight multi-GB probe can't pin the walk — the same
        // promptness the per-probe biased select gave the sequential version. On
        // cancel the remaining in-flight futures are dropped; their orphaned
        // blocking reads finish in the background with results discarded, and the
        // files are re-probed from the `last_path` cursor on resume, so nothing is
        // lost.
        let (pending, res) = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                cancelled = true;
                break 'walk;
            }
            drained = inflight.next() => match drained {
                Some(d) => d,
                None => continue 'walk,
            }
        };

        let (hash, metadata) = match res {
            Ok(hm) => hm,
            Err(e) => {
                warn!(path = %pending.user_path, error = %e, "crawl: probe failed; emitting hashless");
                probe_errors += 1;
                if probe_errors <= MAX_STREAMED_DIAGNOSTICS {
                    crawl_log(
                        &event_stream,
                        LogLevel::Warn,
                        format!(
                            "indexed without metadata (probe failed): {} — {e}",
                            pending.user_path
                        ),
                    )
                    .await;
                }
                (None, None)
            }
        };

        let user_path = pending.user_path;
        let size = pending.size;
        batch.push(Observed {
            path: user_path.clone(),
            size,
            mtime: pending.mtime,
            uid: pending.uid,
            gid: pending.gid,
            mode: pending.mode,
            hash,
            metadata,
        });
        total += 1;
        last_path = Some(user_path);

        // Feed the background sampler. `files` is exact; `bytes` is owned by the
        // sampler (from rchar) on Linux, so only add it here on the non-rchar
        // fallback — the bytes were read by the probe regardless of hash result.
        progress.files.store(total, Ordering::Relaxed);
        if !rchar_active {
            progress.bytes.fetch_add(size, Ordering::Relaxed);
        }

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
            if let Some(max) = max_batches {
                if batch_idx >= max {
                    stopped_by_max = true;
                    break 'walk;
                }
            }

            // Honor cancellation between batches — stop gracefully, reporting
            // what we crawled so far (the caller resumes from `last_path`).
            if cancel.is_cancelled() {
                cancelled = true;
                break 'walk;
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

    // Stop the periodic sampler and emit one final sample, so every walk reports
    // at least once (incl. a sub-`PROGRESS_INTERVAL` crawl the sampler never
    // ticked) and the run closes on the cumulative totals. The rates here are the
    // crawl-wide averages over `crawl_start.elapsed()`.
    drop(_sampler);
    if let (Some(base), Some(now)) = (rchar_baseline, proc_rchar()) {
        progress
            .bytes
            .store(now.saturating_sub(base), Ordering::Relaxed);
    }
    let final_files = progress.files.load(Ordering::Relaxed);
    let final_bytes = progress.bytes.load(Ordering::Relaxed);
    emit_crawl_progress(
        &event_stream,
        file_server.as_deref(),
        final_files,
        final_files,
        final_bytes,
        final_bytes,
        crawl_start.elapsed(),
    )
    .await;

    let exhausted = !cancelled && !stopped_by_max;

    debug!(
        prefix = %config.prefix,
        total,
        batches = batch_idx,
        cancelled,
        exhausted,
        probe_errors,
        list_errors,
        "crawl complete"
    );

    // Surface a completion summary to the RUN log — anomalies especially. Without
    // this the user only sees `count` in the output token and can't tell a clean
    // `count=1` from one where a permission wall silently swallowed the rest. WARN
    // when anything was skipped so it stands out; INFO on a clean walk.
    let mut summary = format!("crawl complete: {total} files indexed");
    if probe_errors > 0 {
        summary.push_str(&format!(
            ", {probe_errors} indexed without metadata (unreadable content)"
        ));
    }
    if list_errors > 0 {
        summary.push_str(&format!(
            ", {list_errors} directories skipped (permission denied / vanished)"
        ));
    }
    if cancelled {
        summary.push_str(", cancelled — will resume");
    } else if stopped_by_max {
        summary.push_str(", paused at max_batches");
    }
    let level = if probe_errors > 0 || list_errors > 0 {
        LogLevel::Warn
    } else {
        LogLevel::Info
    };
    crawl_log(&event_stream, level, summary).await;

    Ok(HashMap::from([
        ("prefix".into(), serde_json::json!(config.prefix)),
        ("count".into(), serde_json::json!(total)),
        ("last_path".into(), serde_json::json!(last_path)),
        ("batches".into(), serde_json::json!(batch_idx)),
        ("cancelled".into(), serde_json::json!(cancelled)),
        ("exhausted".into(), serde_json::json!(exhausted)),
        ("endpoint_root".into(), serde_json::json!(endpoint_root)),
        ("probe_errors".into(), serde_json::json!(probe_errors)),
        ("list_errors".into(), serde_json::json!(list_errors)),
    ]))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::Value;

    use aithericon_executor_backend::traits::EventStream;
    use aithericon_executor_domain::{LogLevel, MetricPoint};
    use aithericon_executor_storage::{StorageBackend, StorageConfig};

    use crate::config::CrawlConfig;

    #[derive(Default)]
    struct CapturingStream {
        items: Mutex<Vec<Value>>,
        logs: Mutex<Vec<(LogLevel, String)>>,
        metrics: Mutex<Vec<MetricPoint>>,
    }

    #[async_trait]
    impl EventStream for CapturingStream {
        async fn log(&self, level: LogLevel, message: String, _fields: HashMap<String, String>) {
            self.logs.lock().unwrap().push((level, message));
        }

        async fn item(&self, _channel: String, _episode_uid: String, _idx: u64, payload: Value) {
            self.items.lock().unwrap().push(payload);
        }

        async fn metric(&self, points: Vec<MetricPoint>) {
            self.metrics.lock().unwrap().extend(points);
        }
    }

    /// Local-backend crawl captures ownership: every emitted item carries the
    /// current process's uid/gid, a mode, and an RFC 3339 mtime — proving the
    /// single-lstat path ran instead of the (ownership-blind) opendal stat.
    #[tokio::test]
    async fn crawl_local_backend_captures_ownership() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nas/sub")).unwrap();
        std::fs::write(dir.path().join("nas/a.txt"), "aaaa").unwrap();
        std::fs::write(dir.path().join("nas/sub/b.txt"), "bb").unwrap();

        let storage = StorageConfig {
            backend: StorageBackend::Local,
            endpoint: dir.path().to_str().unwrap().to_string(),
            bucket: String::new(),
            region: None,
            prefix: String::new(),
            credentials: Default::default(),
            retry: Default::default(),
            resource_alias: None,
        };
        let operator = aithericon_executor_storage::build_operator(&storage).unwrap();
        let endpoint_root = storage.endpoint_root();
        let config = CrawlConfig {
            prefix: "nas/".into(),
            storage,
            batch_size: 10.into(),
            resume_from: None,
            stat: true,
            max_batches: None,
            sink: None,
            probe: None,
            probe_concurrency: 8.into(),
        };

        let stream = Arc::new(CapturingStream::default());
        let run_dir = tempfile::tempdir().unwrap();
        let result = execute(
            &config,
            &operator,
            "",
            &endpoint_root,
            Some(stream.clone()),
            None,
            "exec-test",
            run_dir.path(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["count"], serde_json::json!(2));
        assert_eq!(result["probe_errors"], serde_json::json!(0));

        let root_meta = std::fs::metadata(dir.path()).unwrap();
        let (my_uid, my_gid) = (root_meta.uid(), root_meta.gid());

        let items = stream.items.lock().unwrap();
        let entries: Vec<&Value> = items
            .iter()
            .flat_map(|p| p["items"].as_array().expect("items array"))
            .collect();
        assert_eq!(entries.len(), 2);
        for e in &entries {
            assert!(e["size"].as_u64().unwrap() > 0, "stat size: {e}");
            assert_eq!(e["uid"].as_u64().unwrap() as u32, my_uid, "uid: {e}");
            assert_eq!(e["gid"].as_u64().unwrap() as u32, my_gid, "gid: {e}");
            assert!(e["mode"].as_u64().is_some(), "mode present: {e}");
            let mtime = e["mtime"].as_str().expect("mtime string");
            assert!(
                chrono::DateTime::parse_from_rfc3339(mtime).is_ok(),
                "mtime must parse as RFC 3339: {mtime}"
            );
            // Probing off (default) — no content identity captured.
            assert!(e["hash"].is_null(), "hash absent when probe off: {e}");
            assert!(
                e["metadata"].is_null(),
                "metadata absent when probe off: {e}"
            );
        }
    }

    /// Every probing crawl emits the throughput metrics — including the
    /// `crawl/bytes_*` pair added so a long single-file probe shows live
    /// progress. The final sample always fires (even for this sub-interval
    /// walk), and `bytes_total` reflects the content actually read.
    #[tokio::test]
    async fn crawl_emits_bytes_throughput_metrics() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nas")).unwrap();
        std::fs::write(dir.path().join("nas/a.txt"), vec![b'x'; 4096]).unwrap();
        std::fs::write(dir.path().join("nas/b.txt"), vec![b'y'; 2048]).unwrap();

        let storage = local_storage(dir.path());
        let operator = aithericon_executor_storage::build_operator(&storage).unwrap();
        let endpoint_root = storage.endpoint_root();
        let config = crawl_config(storage, "hash");
        let stream = Arc::new(CapturingStream::default());
        let run_dir = tempfile::tempdir().unwrap();
        let result = execute(
            &config,
            &operator,
            "",
            &endpoint_root,
            Some(stream.clone()),
            None,
            "exec-test",
            run_dir.path(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["count"], serde_json::json!(2));

        let metrics = stream.metrics.lock().unwrap();
        let names: std::collections::HashSet<&str> =
            metrics.iter().map(|m| m.name.as_str()).collect();
        for expected in [
            "crawl/files_per_second",
            "crawl/files_total",
            "crawl/bytes_per_second",
            "crawl/bytes_total",
        ] {
            assert!(
                names.contains(expected),
                "missing metric {expected}: {names:?}"
            );
        }
        // The final cumulative sample must reflect the bytes actually ingested by
        // the probe (≥ the 6 KiB of file content on the per-file fallback path; on
        // Linux the rchar basis is ≥ that too).
        let bytes_total = metrics
            .iter()
            .filter(|m| m.name == "crawl/bytes_total")
            .map(|m| m.value)
            .next_back()
            .expect("a bytes_total sample");
        assert!(
            bytes_total >= 6144.0,
            "bytes_total should cover read content, got {bytes_total}"
        );
        let files_total = metrics
            .iter()
            .filter(|m| m.name == "crawl/files_total")
            .map(|m| m.value)
            .next_back()
            .unwrap();
        assert_eq!(files_total, 2.0, "files_total final sample");
    }

    /// A cancel stops a PROBING crawl at the probe boundary instead of waiting
    /// out the in-flight read — the walk races each `probe_entry` against the
    /// token (a real crawl's multi-GB probe would otherwise block for minutes,
    /// leaving the cancel unhonored and the JetStream job un-acked). Pre-cancel
    /// proves the biased cancel arm wins before the first probe runs: the walk
    /// returns `cancelled=true` having indexed nothing.
    #[tokio::test]
    async fn crawl_cancel_during_probe_stops_walk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nas")).unwrap();
        std::fs::write(dir.path().join("nas/a.txt"), "data").unwrap();
        std::fs::write(dir.path().join("nas/b.txt"), "more").unwrap();

        let storage = local_storage(dir.path());
        let operator = aithericon_executor_storage::build_operator(&storage).unwrap();
        let endpoint_root = storage.endpoint_root();
        let config = crawl_config(storage, "hash");
        let cancel = CancellationToken::new();
        cancel.cancel(); // cancelled before the walk reaches its first probe
        let run_dir = tempfile::tempdir().unwrap();
        let result = execute(
            &config,
            &operator,
            "",
            &endpoint_root,
            None,
            None,
            "exec-test",
            run_dir.path(),
            &cancel,
        )
        .await
        .unwrap();
        assert_eq!(result["cancelled"], serde_json::json!(true));
        assert_eq!(
            result["count"],
            serde_json::json!(0),
            "a pre-cancelled probing walk indexes nothing"
        );
    }

    fn local_storage(dir: &std::path::Path) -> StorageConfig {
        StorageConfig {
            backend: StorageBackend::Local,
            endpoint: dir.to_str().unwrap().to_string(),
            bucket: String::new(),
            region: None,
            prefix: String::new(),
            credentials: Default::default(),
            retry: Default::default(),
            resource_alias: None,
        }
    }

    fn crawl_config(storage: StorageConfig, probe: &str) -> CrawlConfig {
        CrawlConfig {
            prefix: "nas/".into(),
            storage,
            batch_size: 10.into(),
            resume_from: None,
            stat: true,
            max_batches: None,
            sink: None,
            probe: Some(probe.to_string()),
            probe_concurrency: 8.into(),
        }
    }

    async fn run_probed(dir: &tempfile::TempDir, probe: &str) -> (Value, Vec<Value>) {
        let storage = local_storage(dir.path());
        let operator = aithericon_executor_storage::build_operator(&storage).unwrap();
        let endpoint_root = storage.endpoint_root();
        let config = crawl_config(storage, probe);
        let stream = Arc::new(CapturingStream::default());
        let run_dir = tempfile::tempdir().unwrap();
        let result = execute(
            &config,
            &operator,
            "",
            &endpoint_root,
            Some(stream.clone()),
            None,
            "exec-test",
            run_dir.path(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let items = stream.items.lock().unwrap();
        let entries: Vec<Value> = items
            .iter()
            .flat_map(|p| p["items"].as_array().expect("items array"))
            .cloned()
            .collect();
        (serde_json::to_value(&result).unwrap(), entries)
    }

    /// `probe: "hash"` — every item carries the file's SHA-256 (bare lowercase
    /// hex) and NO metadata blob; the digest matches an independent compute.
    #[tokio::test]
    async fn crawl_probe_hash_emits_sha256() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nas")).unwrap();
        std::fs::write(dir.path().join("nas/a.txt"), "hello crawl").unwrap();

        let (result, entries) = run_probed(&dir, "hash").await;
        assert_eq!(result["probe_errors"], serde_json::json!(0));
        assert_eq!(entries.len(), 1);
        let hash = entries[0]["hash"].as_str().expect("hash string");
        let expected = aithericon_file_metadata::compute_checksum(
            &dir.path().join("nas/a.txt"),
            aithericon_file_metadata::ChecksumAlgorithm::Sha256,
        )
        .unwrap()
        .digest;
        assert_eq!(hash, expected, "bare-hex sha256 digest");
        assert!(entries[0]["metadata"].is_null(), "hash mode emits no blob");
    }

    /// `probe: "full"` — items carry the fmeta blob (with its own checksum
    /// matching the item hash); an unmodeled format (.bin) degrades to
    /// checksum-only rather than erroring.
    #[tokio::test]
    async fn crawl_probe_full_emits_metadata_blob() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nas")).unwrap();
        std::fs::write(dir.path().join("nas/t.csv"), "a,b\n1,2\n").unwrap();
        std::fs::write(dir.path().join("nas/x.bin"), [0u8, 1, 2, 3]).unwrap();

        let (result, entries) = run_probed(&dir, "full").await;
        assert_eq!(result["probe_errors"], serde_json::json!(0));
        assert_eq!(entries.len(), 2);
        for e in &entries {
            let hash = e["hash"].as_str().expect("hash present in full mode");
            let blob = &e["metadata"];
            assert!(blob.is_object(), "metadata blob present: {e}");
            assert_eq!(
                blob["checksum"]["digest"].as_str().unwrap(),
                hash,
                "blob checksum matches item hash"
            );
        }
        // The CSV got real format detection; the .bin fell back checksum-only.
        let csv = entries
            .iter()
            .find(|e| e["path"].as_str().unwrap().ends_with(".csv"))
            .unwrap();
        assert_eq!(csv["metadata"]["num_rows"], serde_json::json!(1));
    }

    /// An unreadable file is counted in `probe_errors` and emitted hashless —
    /// the walk itself never fails on probe errors.
    #[tokio::test]
    async fn crawl_probe_error_is_counted_not_fatal() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nas")).unwrap();
        std::fs::write(dir.path().join("nas/ok.txt"), "fine").unwrap();
        let locked = dir.path().join("nas/locked.txt");
        std::fs::write(&locked, "secret").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let (result, entries) = run_probed(&dir, "hash").await;
        // Root runs (CI containers) can read 0o000 files — accept either a
        // counted error or a successful probe, but the walk must include BOTH
        // files and never fail.
        assert_eq!(result["count"], serde_json::json!(2));
        assert_eq!(entries.len(), 2);
        let errs = result["probe_errors"].as_u64().unwrap();
        let locked_entry = entries
            .iter()
            .find(|e| e["path"].as_str().unwrap().ends_with("locked.txt"))
            .unwrap();
        if errs == 1 {
            assert!(
                locked_entry["hash"].is_null(),
                "failed probe emits hashless"
            );
        } else {
            assert_eq!(errs, 0, "either counted or readable, never fatal");
        }

        // Restore perms so TempDir cleanup works everywhere.
        let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644));
    }

    /// A permission-denied directory is skipped (counted in `list_errors`) AND
    /// surfaced to the RUN log: a streamed WARN names the skipped directory and
    /// the completion summary reports the count — so a permission wall is never
    /// the silent "finished with N files" the old crawl produced. The walk still
    /// completes and includes the readable sibling. A completion summary streams
    /// unconditionally (INFO on a clean walk, WARN when anything was skipped).
    #[tokio::test]
    async fn crawl_permission_denied_dir_streams_warning_and_summary() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nas/secret")).unwrap();
        std::fs::write(dir.path().join("nas/ok.txt"), "fine").unwrap();
        std::fs::write(dir.path().join("nas/secret/hidden.txt"), "nope").unwrap();
        // Lock the subdir so the recursive lister can't read its entries.
        let secret = dir.path().join("nas/secret");
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();

        let storage = local_storage(dir.path());
        let operator = aithericon_executor_storage::build_operator(&storage).unwrap();
        let endpoint_root = storage.endpoint_root();
        let config = crawl_config(storage, "hash");
        let stream = Arc::new(CapturingStream::default());
        let run_dir = tempfile::tempdir().unwrap();
        let result = execute(
            &config,
            &operator,
            "",
            &endpoint_root,
            Some(stream.clone()),
            None,
            "exec-test",
            run_dir.path(),
            &CancellationToken::new(),
        )
        .await
        .expect("a permission-denied subdir must not fail the walk");

        // The readable sibling is always crawled (≥1; root reads the locked dir too).
        assert!(result["count"].as_u64().unwrap() >= 1);

        let logs = stream.logs.lock().unwrap();
        assert!(
            logs.iter().any(|(_, m)| m.starts_with("crawl complete:")),
            "a completion summary must always stream: {logs:?}"
        );

        let list_errors = result["list_errors"].as_u64().unwrap();
        if list_errors > 0 {
            // Non-root: the locked dir is skipped and surfaced — a per-error WARN
            // plus the summary naming the skipped count, both at WARN.
            assert!(
                logs.iter().any(|(l, m)| matches!(l, LogLevel::Warn)
                    && m.contains("skipped unreadable directory")),
                "permission-denied dir must stream a WARN line: {logs:?}"
            );
            assert!(
                logs.iter()
                    .any(|(l, m)| matches!(l, LogLevel::Warn) && m.contains("directories skipped")),
                "completion summary must flag skipped dirs at WARN: {logs:?}"
            );
        } else {
            // Root (CI) reads everything → clean INFO summary, no skip warnings.
            assert!(
                logs.iter()
                    .any(|(l, m)| matches!(l, LogLevel::Info) && m.starts_with("crawl complete:")),
                "a clean walk streams an INFO summary: {logs:?}"
            );
        }

        // Restore perms so TempDir cleanup works everywhere.
        let _ = std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o755));
    }

    /// Unknown probe values are a config error (caught at execute too, not
    /// just decl-time validate).
    #[tokio::test]
    async fn crawl_probe_rejects_unknown_mode() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nas")).unwrap();
        let storage = local_storage(dir.path());
        let operator = aithericon_executor_storage::build_operator(&storage).unwrap();
        let endpoint_root = storage.endpoint_root();
        let config = crawl_config(storage, "checksum");
        let run_dir = tempfile::tempdir().unwrap();
        let err = execute(
            &config,
            &operator,
            "",
            &endpoint_root,
            None,
            None,
            "exec-test",
            run_dir.path(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("probe must be"), "{err}");
    }
}
