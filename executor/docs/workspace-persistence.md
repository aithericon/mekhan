# Workspace Persistence on Ephemeral Compute

Design analysis for running the executor on ephemeral compute (Nomad batch, Kubernetes pods without PVCs, spot instances) where local disk does not survive restarts.

**Status:** Design exploration (not yet implemented)

## Problem Statement

The executor currently assumes local disk is available for the duration of an execution: the staging pipeline writes inputs to a run directory, the child process reads and writes files there, and artifacts are collected after exit. All execution state is in-memory.

On ephemeral compute without persistent volumes, this works for a single uninterrupted execution. But if the executor crashes mid-execution, everything accumulated on local disk is lost. NATS redelivers the job, and the new executor instance starts from scratch — re-staging inputs, re-executing the child, re-collecting artifacts.

For short batch jobs this is acceptable. For long-running executions or interactive sessions (e.g., Jupyter notebooks), re-execution from scratch is either expensive or meaningless.

## Current Assumptions That Break

| Assumption | Where | Impact on Ephemeral Compute |
|---|---|---|
| Run directory persists during execution | `CreateRunDirectoryHook` in staging.rs | Fine — ephemeral disk is available *during* execution |
| Artifacts available on local disk after child exits | `ipc_sidecar.rs` artifact handler | Fine if executor doesn't crash before upload |
| Cleanup runs after terminal status | `handler.rs` cleanup logic | Irrelevant — disk is gone anyway on new allocation |
| IPC socket is a local Unix socket | `ipc_sidecar.rs` socket setup | Fine — socket lifetime matches execution lifetime |
| No state survives executor restart | No recovery logic exists | Problematic for long-running or interactive workloads |

## Persistence Approaches

### 1. Persistent Volumes (NFS, EBS, Ceph RBD)

Mount a network volume to the run directory. Files persist across restarts.

**Pros:**
- No SDK changes, no new infrastructure code
- Full POSIX semantics
- Works with every tool and library

**Cons:**
- NFS performance is mediocre for small-file-heavy workloads (notebook autosave, many small Python files) — latency on every `open()`/`fsync()`
- Volume lifecycle management adds operational complexity (provisioning, cleanup, migration)
- Limits scheduler flexibility — volumes may be AZ-pinned or node-local
- NFS server is a single point of failure unless using a distributed filesystem (Ceph, GlusterFS)
- Requires infrastructure that may not exist in all deployment environments

**Best for:** Environments where NFS is already available and the operational overhead is acceptable.

### 2. S3 FUSE Mount (s3fs, goofys, mountpoint-s3)

Mount an S3 bucket as a local filesystem via FUSE.

**Pros:**
- No SDK changes — transparent to applications
- No new infrastructure beyond S3

**Cons:**
- No real POSIX semantics — `rename()` is copy+delete, `fsync()` is a full upload, `append()` may re-upload the entire object
- Jupyter autosave (tmp-write + rename) triggers two full S3 round trips on some implementations
- Random reads into large files are expensive (S3 GET is not seekable without range requests)
- Failure modes are confusing — network blips surface as I/O errors that applications don't handle gracefully
- AWS's own `mountpoint-s3` explicitly documents it is "not suitable for workloads that need full POSIX compliance"

**Best for:** Read-only or read-heavy access to large datasets. Not recommended as a primary workspace for interactive use.

### 3. Content-Defined Chunking / Workspace Sync (e.g., rustic/restic)

Keep a fast local scratch filesystem for the working directory. Run a sidecar process that watches for file changes and incrementally syncs to S3 via content-defined chunking (CDC).

CDC splits files into variable-size chunks using a rolling hash (Rabin fingerprint). Chunk boundaries are determined by content, not position — inserting bytes at the start of a file only changes the first chunk. Only new/modified chunks are uploaded.

**Pros:**
- Full POSIX semantics on local disk — no application compatibility issues
- Incremental uploads — a 50MB notebook with a small cell change uploads only the affected chunks
- Deduplication across snapshots — 100 autosaves of a slowly-changing workspace don't cost 100x storage
- Point-in-time restore — can recover to any previous snapshot
- No infrastructure dependency beyond S3
- Encryption at rest (built into restic format)

**Cons:**
- Crash window — seconds of work can be lost between last sync and crash (tunable via debounce interval)
- Additional sidecar process to operate, monitor, and debug
- Restore time proportional to workspace size on new allocation
- CDC adds code complexity compared to simple file uploads
- `rustic_core` library maturity and API stability need evaluation

**Best for:** Interactive sessions on ephemeral compute where NFS is not available. Provides the best balance of local performance and remote durability.

### 4. Hybrid: NFS for Workspace, S3 for Artifacts

Use NFS for the small, latency-sensitive working directory. Use S3 (via `ArtifactStore`) for large artifacts.

```
/work/          -> NFS (notebooks, scripts, small files)
/data/          -> S3 FUSE read-only mount or pre-staged
/artifacts/     -> local ephemeral, uploaded to S3 via artifact API
```

**Best for:** Production environments with existing NFS infrastructure where simplicity is prioritized over minimizing infrastructure dependencies.

### Comparison

| Approach | New Infrastructure | SDK Changes | POSIX Semantics | Crash Recovery |
|----------|-------------------|-------------|-----------------|----------------|
| NFS | NFS server | None | Full | Full (if NFS survives) |
| S3 FUSE | None | None | Partial | Full (sort of) |
| CDC sync | None (just S3) | Optional hints | Full (local disk) | Seconds of loss |
| NFS + S3 | NFS server | None | Full | Full |

## Batch vs. Interactive Workloads

The executor's current lifecycle model is batch-oriented:

```
Accepted -> Staging -> Running -> [child exits] -> Collecting -> Uploading -> Terminal
```

Interactive sessions (Jupyter, dev environments) have a fundamentally different shape:

```
Accepted -> Staging -> Running -> [stays running indefinitely]
                                    |-- cell execution 1
                                    |-- cell execution 2
                                    |-- ... (hours/days)
                                    +-- [user explicitly stops, or idle timeout]
```

Key differences:

- **Re-execution is not recovery.** A batch job can be re-run from the same inputs. An interactive session holds in-memory state (variables, loaded data, open connections) that cannot be reconstructed by replaying cells.
- **No terminal status for a long time.** The ack_wait / heartbeat model holds a "job slot" for what is really a long-lived service.
- **Artifacts produced continuously.** Users save notebooks, produce plots, and write files throughout the session — not only at exit.
- **Bidirectional connection required.** A batch job is fire-and-forget. A Jupyter session requires a persistent connection (browser -> server -> kernel) that the current NATS pub/sub model doesn't cover.

This suggests a **two-tier model**: the executor handles batch executions; a separate session manager handles interactive session lifecycle. The staging pipeline and backend abstraction are reusable for session provisioning, but the long-lived lifecycle, connectivity, and suspend/resume are separate concerns.

## Design: Execution Session with Phase Gates

For batch workloads that benefit from crash recovery, a lightweight state machine persisted to remote storage enables "resume from last completed phase" semantics.

### Session Record

Stored in NATS KV (preferred, lower latency for the lease pattern) or S3:

```rust
struct ExecutionSession {
    execution_id: String,
    job: ExecutionJob,              // original job, for replay
    phase: SessionPhase,
    staged_inputs: Vec<String>,     // keys of inputs already staged
    uploaded_artifacts: Vec<ArtifactRef>,
    attempt: u32,
    executor_id: String,            // which instance owns this session
    last_heartbeat: DateTime<Utc>,
}

enum SessionPhase {
    Staging,        // downloading inputs, setting up run dir
    Executing,      // child process running
    Collecting,     // gathering outputs/artifacts post-exit
    Uploading,      // pushing artifacts to final storage
    Terminal,       // status published, done
}
```

### Recovery Logic

On job delivery (fresh or redelivered), the handler checks for an existing session:

```
match existing_session.phase {
    None        -> create session, run normal pipeline
    Staging     -> re-stage from scratch (inputs are idempotent downloads)
    Executing   -> cannot resume a dead process; re-stage, re-execute
    Collecting  -> local outputs are gone; re-execute
    Uploading   -> check which artifacts are in final storage, upload remaining
    Terminal    -> re-publish status (NATS dedup makes this safe), ack job
}
```

On ephemeral compute, `Executing` and `Collecting` both collapse to "re-execute" because local disk is gone. The session record's value is primarily:

1. **Skipping duplicate uploads** — artifacts already in S3 from a previous attempt are not re-uploaded.
2. **Preventing contradictory terminal status** — if a previous attempt published `Completed`, don't re-execute and potentially publish `Failed`.
3. **Observability** — session records provide audit trail across attempts.

### Executor Identity and Lease

NATS may redeliver a job to a different executor instance while the original is still running (zombie, network partition, slow GC). The session record acts as a distributed lease:

```rust
if session.executor_id != self.id
   && session.last_heartbeat > now - threshold {
    // Another executor may still be running this — NACK and back off
}
```

NATS KV with TTL provides natural lease semantics.

## Design: Workspace Sync via CDC

For interactive sessions or long-running executions where file durability matters, a workspace sync sidecar incrementally persists working files to S3.

### Architecture

```
+----------------------------------------------+
|  Ephemeral Compute (Nomad batch)             |
|                                              |
|  +---------+    +----------------------+     |
|  | Jupyter  |    | Workspace Sync       |    |
|  | Server   |    | Sidecar              |    |
|  |          |    |                      |     |
|  | /work/<--+----+  notify (fswatch)   |     |
|  |          |    |  debounce (2-5s)    |     |
|  +---------+    |  rustic snapshot     |     |
|                  |  -> S3 chunk upload  |     |
|                  |  -> update snapshot  |     |
|                  +----------+-----------+     |
+----------------------------------------------+
                              |
                              v
                   +-----------------------+
                   |  S3                    |
                   |  /repo/{session_id}/   |
                   |    /chunks/...         |
                   |    /snapshots/...      |
                   |    /index/...          |
                   +-----------------------+
```

The sidecar:

1. **Watches** the workspace directory via the `notify` crate (inotify on Linux, kqueue on macOS).
2. **Debounces** — waits for a quiet period (2-5s) after the last write. Jupyter writes atomically (tmp + rename), so snapshots see consistent state.
3. **Snapshots** via `rustic_core` — walks the tree, chunks new/modified files, uploads only new chunks to S3.
4. **Records snapshot metadata** — the snapshot tree (files -> chunk references) is small and stored in S3.

On restore (new allocation):

```
rustic restore --target /work/ latest
```

Downloads only the chunks needed, reconstructs the workspace. If some chunks are locally cached (warm start), they're skipped.

### CDC Efficiency by File Type

| File Type | Typical Size | Change Pattern | CDC Benefit |
|-----------|-------------|----------------|-------------|
| `.ipynb` notebook | 100KB-50MB | Small edits, new cell outputs appended | High |
| CSV/parquet data | MB-GB | Append, or replace entirely | High for append |
| Model weights | MB-GB | Replace entirely on retrain | Low per-file, dedup across snapshots |
| Python scripts | KB | Small edits | Minimal — whole file is one chunk |
| Generated plots | KB-MB | Replace entirely | Minimal |

## Integration with the Artifact API

Workspace sync and the artifact API serve different purposes:

| | Workspace Sync | Artifact API |
|---|---|---|
| Question answered | "Can I recover my files after crash?" | "What did this execution produce?" |
| Trigger | Filesystem change (implicit) | SDK call (explicit) |
| Scope | Everything in workspace | Files the child declares |
| Metadata | None — just files | Category, MIME type, user KV pairs |
| Discoverability | Opaque (restore whole workspace) | Manifest with queryable records |
| Audience | Same session on recovery | Downstream systems, UI, other executions |

### Unified Storage Model

The principle: **workspace sync owns file persistence, the artifact API owns metadata.**

When a child calls `LogArtifactRequest`:

**Current flow:**
```
sidecar receives LogArtifactRequest { path: "artifacts/model.pt", ... }
  -> artifact_store.upload(execution_id, artifact, local_path)
    -> copies file to s3://artifacts/{execution_id}/{artifact_id}/model.pt
  -> artifact.storage_path = "artifacts/..."
  -> publish ArtifactLogged event
```

**With workspace sync:**
```
sidecar receives LogArtifactRequest { path: "artifacts/model.pt", ... }
  -> workspace_sync.pin_file("artifacts/model.pt")
    -> forces immediate CDC snapshot of that file (skip debounce)
    -> returns snapshot_id + path reference
  -> artifact.content_ref = ContentRef { snapshot: "abc123", path: "artifacts/model.pt" }
  -> save artifact metadata to manifest
  -> publish ArtifactLogged event
```

The file is not copied twice. The workspace sync is the single path to S3 for file content.

### Proposed Trait Split

```rust
/// Owns durable file content (CDC chunks in S3)
pub trait WorkspaceStore {
    /// Force-sync a specific file, returns a durable reference
    async fn pin_file(&self, path: &Path) -> Result<ContentRef>;

    /// Restore a file from a content reference
    async fn restore_file(&self, content_ref: &ContentRef, dest: &Path) -> Result<()>;

    /// Restore entire workspace from latest snapshot
    async fn restore_latest(&self, dest: &Path) -> Result<SnapshotId>;

    /// List available snapshots
    async fn snapshots(&self) -> Result<Vec<SnapshotInfo>>;
}

/// Owns artifact metadata, references WorkspaceStore for content
pub trait ArtifactRegistry {
    async fn register(&self, exec_id: &str, artifact: Artifact,
                      content: ContentRef) -> Result<Artifact>;
    async fn resolve(&self, artifact: &Artifact) -> Result<ContentRef>;
    async fn load_manifest(&self, exec_id: &str) -> Result<Option<ArtifactManifest>>;
    async fn save_manifest(&self, exec_id: &str, manifest: &ArtifactManifest) -> Result<()>;
}

/// Content-addressed reference to a file in the workspace store
pub struct ContentRef {
    pub snapshot_id: String,
    pub path: String,           // relative path within snapshot
    pub size_bytes: u64,
    pub content_hash: String,   // integrity verification
}
```

The existing `ArtifactStore` trait can be preserved as a facade composing `WorkspaceStore` + `ArtifactRegistry`. The `LocalArtifactStore` continues to work unchanged for development and non-ephemeral environments.

### ContentRef Resolution for Downstream Consumers

During execution, artifacts reference CDC snapshots via `ContentRef`. Downstream consumers (UI, other jobs) should not need to understand CDC. On terminal status, artifacts are "finalized":

```rust
// On terminal status (Completed/Failed):
for artifact in manifest.artifacts {
    let content = workspace_store.read_file(&artifact.content_ref).await?;
    let final_path = format!("artifacts/{}/{}/{}", exec_id, artifact.id, artifact.filename);
    s3.put_object(final_path, content).await?;
    artifact.storage_path = Some(final_path);
}
artifact_registry.save_manifest(exec_id, &manifest).await?;
```

During the session, the workspace store is the authority. After completion, artifacts are materialized to simple S3 keys for consumer access.

### SDK Additions for Workspace Hints

The child-side SDK remains unchanged for basic artifact logging. Optional new IPC messages allow workspace sync hints:

```
table WorkspacePinRequest {
    path: string (required);     // relative path to force-sync
    wait_for_sync: bool = false; // block until durably persisted
}

table WorkspaceExcludeRequest {
    patterns: [string] (required); // glob patterns to exclude from sync
}
```

These enable patterns like:

```python
# Ensure a checkpoint is durable before continuing
with executor.workspace.priority_sync():
    torch.save(model.state_dict(), "checkpoints/epoch_5.pt")

# Exclude large temp files from sync
executor.workspace.exclude("tmp/")
```

## Logs and Metrics

Logs and metrics are unaffected by workspace persistence. They already flow through NATS in real time:

- **Logs:** `LogMessageRequest` -> sidecar batching -> `LogSink` (NATS, Loki, file)
- **Metrics:** `LogMetricsRequest` -> sidecar -> `MetricSink` (NATS)

These survive executor crashes because they are published to NATS as they arrive. Workspace sync is purely about files on disk.

Stdout/stderr, currently captured in the in-memory `TailBuffer` ring buffer, could optionally be streamed to NATS events (`executor.events.{id}.output`) for durability. This is a separate concern from workspace sync.

## Recommended Prioritization

1. **Accept re-execution (current model).** For short batch jobs, crash -> redelivery -> re-run is fine. No code changes needed.

2. **Incremental artifact upload via IPC sidecar.** Upload artifacts to S3 as they arrive via `LogArtifactRequest`, rather than collecting after exit. This ensures declared artifacts survive crashes. Moderate effort, high value. Works within the existing `ArtifactStore` trait.

3. **Execution session record.** Persist phase transitions to NATS KV. On redelivery, skip completed phases. Prevents duplicate status publishing and redundant artifact uploads.

4. **CDC workspace sync.** Build the workspace sync sidecar with `rustic_core` when interactive sessions on ephemeral compute become a concrete requirement. This is the right architecture but the highest implementation cost.

For environments where NFS is available, persistent volumes remain the simplest option and should be used. The CDC approach is specifically for environments where persistent volumes are not practical.
