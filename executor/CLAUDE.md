# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Development Commands

This project uses `just` as a task runner. Key commands:

```bash
just build              # Build all crates
just check              # Type-check all crates
just fmt                # Format all code
just lint               # Clippy with -D warnings
just test               # Run all workspace tests
just test-crate <name>  # Test a specific crate (e.g., just test-crate aithericon-executor-backend)
just test-integration   # Integration tests (requires Docker for testcontainers)
just run                # Run the executor service
just run-debug          # Run with RUST_LOG=debug
just clean              # Remove build artifacts
```

NATS (required runtime dependency):
```bash
just nats-up        # Start NATS JetStream in Docker
just nats-down      # Stop NATS container
just nats-subscribe # Subscribe to executor.status.> for debugging
```

Run a single test by name:
```bash
cargo test -p aithericon-executor-service --test integration -- test_name
```

## External Path Dependencies

Two sibling directories must be present for builds:
- `../apalis` — Forked apalis with custom NATS backend (`apalis-nats`)
- `../file-metadata` — `aithericon-file-metadata` crate (always-on dependency of `executor-storage`)

## Architecture

Distributed task executor: receives jobs via NATS JetStream (apalis job queue), dispatches to pluggable backends, publishes structured status updates back via NATS.

### Workspace Crates (dependency order)

1. **executor-domain** — Pure data types with no I/O. `ExecutionJob`, `ExecutionSpec` (tagged enum), `ExecutionStatus` lifecycle, `StatusUpdate`, `ExecutionResult`, `ExecutorError`. All types derive Serialize/Deserialize.

2. **executor-ipc** — FlatBuffers-based IPC protocol over Unix sockets. Length-prefixed framing (4-byte LE + FlatBuffers message, 16MB max). Used for child process communication during execution.

3. **executor-storage** — `ArtifactStore` trait with `LocalArtifactStore` (filesystem) and optional `OpenDalArtifactStore` (S3/GCS/Azure, feature-gated). Handles artifact upload/download and metadata.

4. **executor-backend** — `ExecutionBackend` trait and `ProcessBackend` implementation. Spawns local processes with piped I/O, timeout handling (SIGTERM → 5s grace → SIGKILL), cancellation via `CancellationToken`, bounded output capture via `TailBuffer` ring buffer.

5. **executor-metrics** — `MetricSink` trait with in-memory, NATS, and composite sink implementations. Publishes to `EXECUTOR_METRICS` stream when NATS sink is enabled.

6. **executor-worker** — Orchestration layer:
   - `ExecutorConfig` — Layered config via config-rs (defaults → `executor.toml` → `EXECUTOR_*` env vars)
   - `BackendRegistry` — Dispatches `ExecutionSpec` variants to the appropriate backend
   - `StatusReporter` — Publishes `StatusUpdate` to NATS JetStream with idempotent `Nats-Msg-Id` headers
   - `handle_execution` — The apalis job handler: Accepted → find backend → staging pipeline → IPC sidecar → execute → report terminal status
   - Staging pipeline — Ordered hooks: `CreateRunDirectoryHook` → `InjectEnvironmentHook` → `StageInputsHook` → `WriteContextHook` → `backend.prepare()`
   - IPC sidecar — Listens on `{run_dir}/ipc.sock` for child process messages (artifacts, progress, logs)
   - Cancellation — `CancellationRegistry` with `NatsCancelListener` on `executor.cancel.*`

7. **executor-service** — Binary entry point. Three operating modes controlled by orthogonal config axes (`JobSource` × `Lifetime`):
   - `nats_queue` + `daemon` — Long-running apalis worker pulling from NATS (default)
   - `nats_queue` + `run_to_completion` — **Drain mode**: pull from NATS queue, process up to `max_jobs`, exit on limit or idle timeout
   - `manifest` + `run_to_completion` — Push manifest jobs through the same apalis pipeline, collect results, exit

   All paths use the same apalis worker (`build_worker!` macro), so jobs get ack timeout, heartbeats, and the full handler lifecycle. Drain mode uses `CompletionTracker` (atomic counter + watch channel) to signal shutdown via `drain_signal()`. `BatchRunner` pushes manifest jobs into NatsStorage and monitors the JetStream status stream for terminal results.

8. **executor-kreuzberg** — Document text extraction backend via the kreuzberg Rust library. Extracts text, metadata, and tables from 75+ file formats (PDF, Office, images w/ OCR, email, archives). Supports single-file and batch extraction modes with per-file progress. Feature-gated: `kreuzberg`.

9. **executor-test-harness** — Integration test utilities. `ExecutorTestContext` provides per-test NATS stream isolation via UUID prefixes, testcontainers NATS setup, and helpers (`echo_job()`, `failing_job()`, `sleep_job()`, etc.).

### Key Design Decisions

- **Execution failures are not infra errors** — `handle_execution` returns `Ok(())` even when a process fails. Only infrastructure issues (NATS down, backend not found) are `Err`. This prevents apalis from retrying application-level failures.
- **Backends never touch NATS** — Status reporting is abstracted via `StatusCallback` closure passed to `backend.execute()`. This keeps executor-backend free of NATS dependencies.
- **Bounded memory** — stdout/stderr captured via TailBuffer ring buffer (default 64KB), not unbounded Vecs.
- **Metadata pass-through** — `ExecutionJob.metadata` (opaque HashMap) is echoed in all `StatusUpdate` messages for caller-side routing/correlation.
- **Status deduplication** — NATS `Nats-Msg-Id` header set to `{execution_id}-{status}` ensures idempotent publishing.

### NATS Streams & Subjects

| Stream | Subject Pattern | Purpose |
|--------|----------------|---------|
| `EXECUTOR_STATUS` | `executor.status.{execution_id}.{status}` | Status lifecycle updates (Limits retention, 24h max age) |
| `EXECUTOR_EVENTS` | `executor.events.{execution_id}.{category}` | Mid-execution events (artifact, progress, phase, log, output) |
| `EXECUTOR_METRICS` | `executor.metrics.*` | Metric batches |
| — | `executor.cancel.*` | Cancellation requests (plain NATS, not JetStream) |

### Feature Flags (executor-service)

```
opendal         — OpenDAL storage backend base
opendal-s3      — S3 storage (implies opendal)
opendal-gcs     — GCS storage (implies opendal)
opendal-azblob  — Azure Blob storage (implies opendal)
kreuzberg       — Kreuzberg document extraction backend
```

### Configuration

Loaded via config-rs: hardcoded defaults → optional `executor.toml` → `EXECUTOR_*` env vars (case-insensitive, `_` separator for nesting).

Key defaults: NATS at `localhost:4222`, concurrency 4, 1-hour default timeout, 64KB max output, 30s heartbeat interval, max 3 delivery attempts, `immediate` cleanup policy.

Key env vars for mode selection:
- `EXECUTOR_SOURCE` — `nats_queue` (default) or `manifest`
- `EXECUTOR_LIFETIME` — `daemon` (default) or `run_to_completion`
- `EXECUTOR_MANIFEST_PATH` — Path to manifest JSON (required when source=manifest)
- `EXECUTOR_FAIL_FAST` — Stop on first failure in manifest mode (default false)

Drain mode env vars (setting either auto-promotes lifetime to `run_to_completion`):
- `EXECUTOR_MAX_JOBS` — Hard cap on jobs; exit immediately when reached (default unset)
- `EXECUTOR_MIN_JOBS` — Minimum completions before idle shutdown eligible (default unset)
- `EXECUTOR_IDLE_TIMEOUT_SECS` — Idle timeout after min_jobs reached (default 30)

### Testing

Integration tests live in `crates/executor-service/tests/`. Each test gets an isolated NATS stream via `ExecutorTestContext` (UUID-prefixed). Tests require Docker for testcontainers NATS. Key test files: `integration.rs`, `error_paths.rs`, `output_capture.rs`, `concurrency.rs`, `ipc.rs`, `storage_staging.rs`, `cleanup.rs`, `batch.rs`, `drain.rs`.
