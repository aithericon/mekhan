# NATS Contracts

All inter-service communication uses NATS JetStream.

## Status Stream

| Property | Value |
|---|---|
| Stream name | `EXECUTOR_STATUS` (or `STATUS_{prefix}` with prefix) |
| Subjects | `executor.status.>` (or `{prefix}.executor.status.>`) |
| Retention | Limits |
| Max age | 24 hours |
| Dedup window | 2 minutes |
| Storage | File |
| Replicas | Configurable (default 1) |

Limits retention means multiple consumers (monitoring, CLI, petri watcher) can independently read all updates. This is not a work queue — messages are not removed on ack.

### Status Subject Pattern

```
executor.status.{execution_id}.{status}
```

Examples:
```
executor.status.train-alpha-0.accepted
executor.status.train-alpha-0.running
executor.status.train-alpha-0.completed
```

Characters invalid in NATS subject tokens (spaces, `>`, `*`) are replaced with `_`.

### Deduplication

Each status update has a deterministic `Nats-Msg-Id` header:

```
{execution_id}-{status}
```

Example: `train-alpha-0-completed`

Since each execution transitions through each status at most once, this prevents duplicate publications within the 2-minute dedup window.

## StatusUpdate

Published on every status transition.

```json
{
  "execution_id": "train-alpha-0",
  "status": "completed",
  "detail": {
    "outcome": { "type": "success" },
    "duration_ms": 5000,
    "stdout_tail": "Training complete.\n",
    "stderr_tail": null
  },
  "metadata": {
    "petri_net_id": "net-1",
    "transition_id": "t-train"
  },
  "source": "executor-hostname",
  "timestamp": "2025-01-15T10:30:00Z"
}
```

| Field | Type | Description |
|---|---|---|
| `execution_id` | `string` | Execution identifier. |
| `status` | `string` | One of: `accepted`, `running`, `completed`, `failed`, `cancelled`, `timed_out`. |
| `detail` | `json` | Status-specific detail (see below). |
| `metadata` | `map<string, string>` | Echoed from `ExecutionJob.metadata`. |
| `source` | `string` | Executor instance name. |
| `timestamp` | `string` | ISO 8601 UTC timestamp. |

### Detail by Status

**accepted:**
```json
{}
```

**running:**
```json
{ "pid": 1234 }
```

**completed:**
```json
{
  "outcome": { "type": "success" },
  "duration_ms": 5000,
  "stdout_tail": "...",
  "stderr_tail": "..."
}
```

**failed:**
```json
{
  "outcome": { "type": "exit_failure", "exit_code": 1 },
  "duration_ms": 1000,
  "stdout_tail": "...",
  "stderr_tail": "Error: segfault\n"
}
```

Outcome types: `success`, `exit_failure` (`exit_code`), `signal` (`signal`), `timed_out`, `backend_error` (`message`), `cancelled`.

**timed_out / cancelled:**
```json
{
  "outcome": { "type": "timed_out" },
  "duration_ms": 3600000,
  "stdout_tail": "...",
  "stderr_tail": "..."
}
```

## Event Stream

Mid-execution events (artifacts, progress, phases, logs, outputs) are published to a separate stream.

| Property | Value |
|---|---|
| Stream name | `EXECUTOR_EVENTS` |
| Subjects | `executor.events.>` |
| Retention | Limits |
| Max age | 24 hours |
| Storage | File |

### Event Subject Pattern

```
executor.events.{execution_id}.{category}
```

Categories: `artifact`, `progress`, `phase`, `log`, `output`, `metric`.

### ExecutionEvent

```json
{
  "execution_id": "train-alpha-0",
  "category": "artifact",
  "detail": {
    "event_type": "artifact_logged",
    "artifact_id": "art-1",
    "name": "model.pt",
    "category": "model"
  },
  "metadata": { "petri_net_id": "net-1" },
  "source": "executor-hostname",
  "timestamp": "2025-01-15T10:25:00Z",
  "sequence": 42
}
```

| Field | Type | Description |
|---|---|---|
| `execution_id` | `string` | Execution identifier. |
| `category` | `string` | Event category (`artifact`, `progress`, `phase`, `log`, `output`, `metric`). |
| `detail` | `StatusDetail` | Typed event detail (tagged on `event_type`). |
| `metadata` | `map<string, string>` | Echoed from job. |
| `source` | `string` | Executor instance name. |
| `timestamp` | `string` | ISO 8601 UTC. |
| `sequence` | `uint64` | Monotonically increasing per execution. |

Dedup via `Nats-Msg-Id`: `{execution_id}-{category}-{sequence}`.

### StatusDetail Variants

| `event_type` | Fields | Description |
|---|---|---|
| `accepted` | — | Job accepted. |
| `running` | `pid` (optional) | Execution started. |
| `artifact_logged` | `artifact_id`, `name`, `category` | Artifact registered via IPC. |
| `progress_updated` | `fraction`, `message`, `current_step`, `total_steps` | Progress update. |
| `phase_changed` | `phase_name`, `status`, `message` | Phase status change. |
| `log_message` | `level`, `message`, `fields` | Structured log from child. |
| `output_set` | `name`, `value` | Output value set via IPC. |
| `completed` | `outcome`, `duration_ms` | Execution succeeded. |
| `failed` | `outcome`, `error`, `duration_ms` | Execution failed. |

## Metrics Stream

Metric batches from executions are published to a dedicated stream.

| Property | Value |
|---|---|
| Stream name | `EXECUTOR_METRICS` |
| Subjects | `executor.metrics.*` |
| Retention | Limits |
| Max age | 24 hours |
| Storage | File |

Each message is a `MetricBatch` containing one or more `MetricPoint` entries. Backends like HTTP and Rig populate `MetricSummary` in `ExecutionResult`; the metric sink publishes full time-series to this stream when the NATS metric sink is enabled.

## Cancel Subject

Cancellation requests use plain NATS (not JetStream) for low-latency delivery.

```
executor.cancel.{execution_id}
```

The `NatsCancelListener` subscribes to `executor.cancel.*` and triggers the `CancellationToken` for matching executions. Any message on the subject triggers cancellation — the payload is ignored.

## Job Stream

Jobs are managed by the apalis-nats backend. The stream namespace is configurable (default: `executor_jobs`). Callers publish `ExecutionJob` JSON to the apalis job stream. apalis handles prioritization, redelivery, and DLQ.
