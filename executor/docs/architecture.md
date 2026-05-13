# Architecture

## Crate Dependency Graph

```
executor-domain          (pure types, no I/O)
  ├── executor-ipc       (FlatBuffers IPC protocol)
  ├── executor-storage   (ArtifactStore trait + local/OpenDAL impl)
  ├── executor-backend   (ExecutionBackend trait + process/docker/python/http backends)
  ├── executor-llm       (LLM backend via direct HTTP — feature-gated: llm)
  ├── executor-file-ops  (file operations backend — feature-gated: file-ops)
  └── executor-kreuzberg (document extraction backend — feature-gated: kreuzberg)
        └── executor-worker    (staging, IPC sidecar, handler, reporter)
              └── executor-service   (binary, NATS wiring)
```

`executor-domain` is at the root — every other crate depends on it.
`executor-worker` pulls in `ipc`, `storage`, and `backend`.
`executor-llm`, `executor-file-ops`, and `executor-kreuzberg` are standalone crates that depend on `executor-domain` and `executor-backend` (for the trait).
`executor-service` is the leaf binary that registers all backends.

## Data Flow

```
                           NATS JetStream
                          ┌──────────────┐
  Caller ──► Job Stream ──│  apalis-nats │──► handle_execution()
                          └──────────────┘         │
                                                   ▼
                                          ┌─────────────────┐
                                          │ StagingPipeline  │
                                          │  1. mkdir        │
                                          │  2. inject env   │
                                          │  3. stage inputs │
                                          │  4. write ctx    │
                                          │  5. backend.     │
                                          │     prepare()    │
                                          └────────┬────────┘
                                                   ▼
                                          ┌─────────────────┐
                                          │ backend.execute()│
                                          │ (any backend)    │
                                          └────────┬────────┘
                                                   │
                          ┌────────────────────────┼────────────────────┐
                          ▼                        ▼                    ▼
                   Child Process            IPC Sidecar          StatusReporter
                   (user code)           (Unix socket)         (NATS publish)
                          │                        │                    ▲
                          └──── FlatBuffers ───────┘                    │
                                                                       │
                                              StatusUpdate ────────────┘
                                              (terminal status)
```

1. A caller publishes an `ExecutionJob` to the NATS job stream.
2. The apalis consumer picks it up and calls `handle_execution`.
3. The handler reports `Accepted`, finds a backend, builds a `RunContext`.
4. The staging pipeline creates the run directory, injects env vars, stages inputs, and writes `context.json`.
5. The backend executes the job (e.g., `ProcessBackend` spawns a process, `HttpBackend` sends an HTTP request, `LlmBackend` calls an LLM, `FileOpsBackend` runs a storage operation, `KreuzbergBackend` extracts document text).
6. The child process communicates with the IPC sidecar via FlatBuffers over a Unix socket.
7. The handler maps the `ExecutionOutcome` to a terminal `ExecutionStatus` and publishes it to NATS.

## Design Decisions

**Execution failures are not infrastructure errors.** `handle_execution` returns `Ok(())` even when a process exits non-zero or times out. Only true infrastructure failures (NATS down, deserialization errors) return `Err` for apalis retry/DLQ. This means the job queue never retries a legitimate execution failure.

**Bounded memory.** stdout and stderr are captured via a `TailBuffer` ring buffer (default 64 KB per stream). No unbounded allocations from process output.

**Metadata pass-through.** `ExecutionJob.metadata` is an opaque `HashMap<String, String>` echoed in every `StatusUpdate`. Callers use it for routing and correlation without the executor needing to understand the contents.

**StatusCallback indirection.** Backends never touch NATS directly. They report status changes through a `StatusCallback` closure provided by the worker. This keeps `executor-backend` free of NATS dependencies.

**apalis local fork.** The job queue uses a [local fork of apalis](https://github.com/geofmureithi/apalis) at `../apalis` with a custom NATS JetStream backend (`apalis-nats`). This sibling directory must be present for builds.
