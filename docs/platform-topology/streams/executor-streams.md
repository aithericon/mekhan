---
type: NATS Stream
title: Executor Streams
description: The executor status, events, and datastream JetStream streams carrying job lifecycle, mid-execution events, and chunked binary output.
tags: [nats, jetstream, executor, streaming]
timestamp: 2026-06-18T00:00:00Z
---

# Executor Streams

Three streams carry job execution telemetry from the [executor](/platform-topology/subjects/executor.md)
back to the engine and mekhan. Status/events are the control plane; datastream is
the data plane (per `docs/35-allocation-and-traffic-planes.md` and
`docs/36-output-data-plane.md`).

Defined service-side in `service/src/streams/mod.rs`; executor-side producers in
`executor/crates/executor-worker/src/{reporter.rs,chunks.rs}`.

# Schema

| Stream | Subjects | Retention | Max age | Plane |
|--------|----------|-----------|---------|-------|
| `EXECUTOR_STATUS` | `executor.status.>` | limits | 24 h | lifecycle (Accepted→Running→Completed/Failed/Cancelled) |
| `EXECUTOR_EVENTS` | `executor.events.>` | limits | 24 h | mid-execution (artifact, progress, log, output, `control_emit`) |
| `EXECUTOR_DATASTREAM` | `executor.datastream.>` | limits | 24 h | chunked binary byte streams (stdout / data channels) |

All carry a 120 s duplicate window.

# Lifecycle

`EXECUTOR_STATUS` and `EXECUTOR_EVENTS` are created at boot. **`EXECUTOR_DATASTREAM`
is created lazily on the first chunked output** — absent on a fresh idle cluster.

# Consumers

The engine drains both control-plane streams via `petri-executor-status`
(on `EXECUTOR_STATUS`) and `petri-executor-events` (on `EXECUTOR_EVENTS`), mapping
results back into the net. See [result ingest](/platform-topology/consumers/result-ingest.md).

# Citations

[1] `service/src/streams/mod.rs`.
[2] `executor/crates/executor-worker/src/reporter.rs`, `.../chunks.rs`.
[3] Live: `nats stream info` — `executor.{status,events}.>`, 24h; `EXECUTOR_DATASTREAM` absent (lazy).
