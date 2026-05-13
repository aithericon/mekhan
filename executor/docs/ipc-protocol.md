# IPC Protocol

The executor communicates with child processes via a FlatBuffers-based protocol over a Unix domain socket.

## Transport

- **Socket**: Unix domain socket at `{run_dir}/ipc.sock`
- **Discovery**: Child processes read the path from `AITHERICON_IPC_SOCKET` environment variable.
- **Connection model**: The sidecar accepts a single connection. The child connects, sends requests, receives responses, then disconnects.

## Framing

Each message is length-prefixed:

```
┌─────────────────────────┬───────────────────┐
│ 4 bytes (LE u32)        │ payload (FlatBuf) │
│ length of payload       │                   │
└─────────────────────────┴───────────────────┘
```

- Length prefix is little-endian unsigned 32-bit integer.
- Maximum message size: 16 MB (`16 * 1024 * 1024` bytes).
- Messages exceeding the limit are rejected with a framing error.

## Protocol Flow

```
Child Process                        IPC Sidecar
     │                                    │
     │──── connect ──────────────────────►│
     │                                    │
     │──── Request (FlatBuffer) ────────►│
     │◄─── Response (FlatBuffer) ────────│
     │                                    │
     │──── Request (FlatBuffer) ────────►│
     │◄─── Response (FlatBuffer) ────────│
     │                                    │
     │    ... repeat ...                  │
     │                                    │
     │──── disconnect ───────────────────►│
     │                                    │
```

Each request gets exactly one response. The sidecar processes requests sequentially.

## FlatBuffers Schema

```flatbuffers
namespace aithericon.executor.ipc;

enum ArtifactCategory : byte { Model, Dataset, Plot, Log, Checkpoint, Config, Metric, Other }
enum PhaseStatus : byte { Pending, Running, Completed, Failed, Skipped }
enum LogLevel : byte { Trace, Debug, Info, Warn, Error }
enum ResponseStatus : byte { Ok, Error, NotFound, InvalidArgument }

table LogArtifactRequest {
    artifact_id: string (required);
    path: string (required);
    name: string;
    category: ArtifactCategory = Other;
    mime_type: string;
    metadata_keys: [string];
    metadata_values: [string];
    extract_file_metadata: bool = true;
}

table UpdateProgressRequest {
    fraction: float;
    message: string;
    current_step: uint64;
    total_steps: uint64;
}

table DefinePhasesRequest {
    phase_names: [string] (required);
}

table UpdatePhaseRequest {
    phase_name: string (required);
    status: PhaseStatus;
    message: string;
}

table LogMessageRequest {
    level: LogLevel = Info;
    message: string (required);
    field_keys: [string];
    field_values: [string];
}

table SetOutputRequest {
    name: string (required);
    value_json: string (required);
}

table HealthCheckRequest { sequence: uint64; }
table ShutdownAckRequest { exit_code: int32; }

union RequestPayload {
    LogArtifactRequest,
    UpdateProgressRequest,
    DefinePhasesRequest,
    UpdatePhaseRequest,
    LogMessageRequest,
    SetOutputRequest,
    HealthCheckRequest,
    ShutdownAckRequest,
}

table Request {
    request_id: uint64;
    payload: RequestPayload (required);
}

table Response {
    status: ResponseStatus = Ok;
    error_message: string;
    request_id: uint64;
}

root_type Request;
```

## Request Types

| Request | Purpose | Key Fields |
|---|---|---|
| `LogArtifactRequest` | Register an artifact file produced by the child | `artifact_id`, `path` (local file), `name`, `category`, `mime_type`, `metadata_keys`/`metadata_values`, `extract_file_metadata` |
| `UpdateProgressRequest` | Report execution progress | `fraction` (0.0–1.0), `message`, `current_step`, `total_steps` |
| `DefinePhasesRequest` | Declare named execution phases | `phase_names` (ordered list) |
| `UpdatePhaseRequest` | Update a phase's status | `phase_name`, `status` (Pending/Running/Completed/Failed/Skipped), `message` |
| `LogMessageRequest` | Send a structured log message | `level` (Trace/Debug/Info/Warn/Error), `message`, `field_keys`/`field_values` |
| `SetOutputRequest` | Set a named output value | `name`, `value_json` (JSON-encoded string) |
| `HealthCheckRequest` | Liveness ping | `sequence` (monotonic counter) |
| `ShutdownAckRequest` | Acknowledge a shutdown signal | `exit_code` |

## Response

Every request receives a `Response`:

| Field | Type | Description |
|---|---|---|
| `request_id` | `uint64` | Echoed from the request for correlation. |
| `status` | `ResponseStatus` | `Ok`, `Error`, `NotFound`, or `InvalidArgument`. |
| `error_message` | `string` | Present when status is not `Ok`. |

## Artifact Logging

When the child sends `LogArtifactRequest`:

1. The sidecar reads the file at `path` (must be a local filesystem path accessible to the sidecar).
2. If an `ArtifactStore` is configured, the file is uploaded via `ArtifactStore.upload()`.
3. The artifact is recorded in the `SidecarResult` and included in the final `ArtifactManifest`.

## SDK Usage Pattern

A Python SDK (or any language) would:

1. Read `AITHERICON_IPC_SOCKET` from environment.
2. Connect to the Unix socket.
3. Build FlatBuffer `Request` messages with incrementing `request_id`.
4. Send each as `[4-byte LE length][payload]`.
5. Read `[4-byte LE length][payload]` response.
6. Parse `Response` FlatBuffer, check `status`.
7. Disconnect when done.
