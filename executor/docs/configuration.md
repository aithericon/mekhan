# Configuration

## Loading Order

Configuration is loaded via `config-rs` in this order (later sources override earlier):

1. **Built-in defaults** (hardcoded in `config.rs`)
2. **Config file** — `executor.toml` in the current working directory (optional)
3. **Environment variables** — `EXECUTOR_` prefix

### Environment Variable Convention

Single underscores are literal — they map directly to field names:

```
EXECUTOR_NATS_URL       → nats_url
EXECUTOR_MAX_JOBS       → max_jobs
EXECUTOR_FAIL_FAST      → fail_fast
```

Double underscores denote nesting into sub-structs:

```
EXECUTOR_CANCEL__NATS       → cancel.nats
EXECUTOR_CANCEL__HTTP_PORT  → cancel.http_port
EXECUTOR_STORAGE__BACKEND   → storage.backend
```

> **Implementation note:** The `config-rs` `Environment` source requires both
> `prefix_separator("_")` and `separator("__")`. Without an explicit
> `prefix_separator`, the crate uses the `separator` value for prefix
> stripping too — meaning it would expect `EXECUTOR__NAME` instead of
> `EXECUTOR_NAME`, silently ignoring all env vars and falling back to
> defaults.

## Configuration Reference

| Env Var | Config Key | Default | Description |
|---|---|---|---|
| `EXECUTOR_BASE_DIR` | `base_dir` | `$HOME/.aithericon/executor` | Root for run directories and artifact storage. |
| `EXECUTOR_NATS_URL` | `nats_url` | `nats://localhost:4222` | NATS server URL. |
| `EXECUTOR_NAME` | `name` | `executor-{hostname}` | Instance name, used as `source` in status updates. |
| `EXECUTOR_NAMESPACE` | `namespace` | `executor_jobs` | apalis job stream namespace. |
| `EXECUTOR_CONCURRENCY` | `concurrency` | `4` | Maximum concurrent jobs. |
| `EXECUTOR_DEFAULT_TIMEOUT_SECS` | `default_timeout_secs` | `3600` (1 hour) | Default job timeout when not specified in the job. |
| `EXECUTOR_MAX_OUTPUT_BYTES` | `max_output_bytes` | `65536` (64 KB) | Max stdout/stderr capture per stream (TailBuffer size). |
| `EXECUTOR_ACK_WAIT_SECS` | `ack_wait_secs` | `120` (2 min) | apalis ack timeout before redelivery. |
| `EXECUTOR_HEARTBEAT_INTERVAL_SECS` | `heartbeat_interval_secs` | `30` | Progress heartbeat interval (extends ack_wait during execution). |
| `EXECUTOR_MAX_DELIVER` | `max_deliver` | `3` | Maximum delivery attempts before DLQ. |
| `EXECUTOR_STATUS_REPLICAS` | `status_replicas` | `1` | JetStream stream replicas for EXECUTOR_STATUS. |

### Operating Mode

| Env Var | Config Key | Default | Description |
|---|---|---|---|
| `EXECUTOR_SOURCE` | `source` | `nats_queue` | Job source: `nats_queue` or `manifest`. |
| `EXECUTOR_LIFETIME` | `lifetime` | `daemon` | Process lifetime: `daemon` or `run_to_completion`. |
| `EXECUTOR_MANIFEST_PATH` | `manifest_path` | *(unset)* | Path to manifest JSON. Required when `source = manifest`. |
| `EXECUTOR_FAIL_FAST` | `fail_fast` | `false` | Stop on first failure in `run_to_completion` mode. |

### Drain Mode (NatsQueue + RunToCompletion)

Setting `max_jobs` or `min_jobs` auto-promotes `lifetime` to `run_to_completion` while keeping `source = nats_queue`. The executor pulls jobs from the NATS queue, processes a bounded number, then exits cleanly.

| Env Var | Config Key | Default | Description |
|---|---|---|---|
| `EXECUTOR_MAX_JOBS` | `max_jobs` | *(unset)* | Hard cap on jobs. Exit immediately when reached. |
| `EXECUTOR_MIN_JOBS` | `min_jobs` | *(unset)* | Minimum completions before idle shutdown is eligible. |
| `EXECUTOR_IDLE_TIMEOUT_SECS` | `idle_timeout_secs` | `30` | Seconds with no completions before idle shutdown (after `min_jobs` met). |

**Shutdown semantics:**

1. **Phase 1** — If `min_jobs` is set, wait until `completed >= min_jobs`.
2. **Phase 2** — Wait until `completed >= max_jobs` (immediate exit) or idle timeout with no new completions.
3. Ctrl+C always works as an escape hatch.

**Mode matrix:**

| Source | Lifetime | Behavior |
|--------|----------|----------|
| `nats_queue` | `daemon` | Long-running worker (default). |
| `nats_queue` | `run_to_completion` | **Drain mode** — pull from queue, exit on limits/idle. |
| `manifest` | `run_to_completion` | Batch mode — run manifest jobs, exit with results. |

### Cancellation

| Env Var | Config Key | Default | Description |
|---|---|---|---|
| `EXECUTOR_CANCEL__NATS` | `cancel.nats` | `true` | Enable NATS cancel listener (`executor.cancel.*`). |
| `EXECUTOR_CANCEL__HTTP` | `cancel.http` | `false` | Enable HTTP cancel endpoint. |
| `EXECUTOR_CANCEL__HTTP_PORT` | `cancel.http_port` | `9090` | Port for the HTTP cancel API. |
| `EXECUTOR_CANCEL__HTTP_BIND` | `cancel.http_bind` | `0.0.0.0` | Bind address for the HTTP cancel API. |

## Example executor.toml

```toml
base_dir = "/data/executor"
nats_url = "nats://nats.internal:4222"
name = "executor-gpu-01"
concurrency = 8
default_timeout_secs = 7200
max_output_bytes = 131072
```

### Drain mode example

```toml
# Process up to 10 jobs from the queue, then exit.
max_jobs = 10
idle_timeout_secs = 60
```

Or via environment variables:

```bash
EXECUTOR_MAX_JOBS=10 EXECUTOR_IDLE_TIMEOUT_SECS=60 cargo run -p aithericon-executor-service
```

## Directory Layout

The `base_dir` is the root for all executor state:

```
{base_dir}/
├── runs/                    # Run directories (one per execution)
│   └── {execution_id}/      # See run-directory.md
└── artifacts/               # Artifact storage (LocalArtifactStore)
    └── {execution_id}/      # See storage.md
```
