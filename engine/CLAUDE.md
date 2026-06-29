# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Petri-Lab is a **Colored Petri Net execution engine** with event sourcing, NATS JetStream streaming, and external resource management. It models workflows as typed Petri nets where tokens carry structured data, transitions have Rhai-scripted guards/logic, and external systems integrate through adapters via NATS.

## Build & Development Commands

Commands use `just` (justfile at repo root) with two submodules. Run `just` to see all recipes.

```bash
# Build & Dev (root)
just build                    # Build core-engine
just check                    # cargo check --workspace
just fmt                      # cargo fmt --all
just lint                     # cargo clippy --workspace -- -D warnings
just run                      # Build + run engine (starts NATS first)
just run-debug                # Run with RUST_LOG=debug

# Tests (root)
just test                     # cargo test --workspace (unit tests)
just test-crate <name>        # cargo test -p <name> (single crate)
just test-nats-rust           # NATS integration tests (testcontainers, runs + cleans up)
just test-nats                # Shell-based E2E integration test
just test-integration         # Full suite (nats-rust + nats)

# SDK (root)
just sdk-build                # Build SDK crate
just sdk-example <name>       # Run SDK example (e.g., just sdk-example resource_allocation)

# Infrastructure (just infra ...)
just infra nats-up            # Start NATS JetStream via Docker Compose
just infra nats-subscribe     # Monitor all petri.events.> subjects
just infra nats-streams       # List JetStream streams
just infra nats-shell         # Interactive nats-box shell
just infra nats-cleanup       # Remove orphaned testcontainer NATS instances
just infra nomad-up           # Start Nomad dev agent
just infra slurm-up           # Start local Slurm cluster

# Demos (just demo ...)
just demo nomad               # Nomad batch net demo
just demo slurm               # Slurm batch net demo
just demo executor            # Executor lifecycle demo
just demo vault-secrets       # Vault secret wrapping demo
just demo three-layer         # Three-layer bridged net (Nomad)
just demo slurm-three-layer   # Three-layer bridged net (Slurm)
just demo workflow            # Four-layer workflow (Nomad)
just demo slurm-workflow      # Four-layer workflow (Slurm)
just demo campaign            # Five-layer campaign (Nomad)
just demo slurm-campaign      # Five-layer campaign (Slurm)
just demo python              # Python IPC demo (Nomad)
just demo slurm-python        # Python IPC demo (Slurm)
just demo timer               # Durable timer demo
just demo human               # Human task demo
just demo expense             # Expense approval demo (Python + Human + Timer)
# Stop any demo with: just demo <name>-stop
```

NATS integration tests use testcontainers (auto-provisioned NATS instances) and run with `--test-threads=1`. Run `just infra nats-cleanup` to remove any orphaned containers after interrupted test runs.

## Workspace Structure

```
Cargo.toml                          # Workspace root (13 members)
core-engine/                        # Main binary (Axum HTTP server, port 3030)
core-engine/crates/
  domain/        (petri-domain)     # Event types, Petri net primitives, DomainEvent enum
  application/   (petri-application)# Evaluation engine, transition firing, Rhai runtime, schema validation
  infrastructure/(petri-infrastructure) # MemoryEventStore, MemoryTopologyStore, MarkingProjection
  api/           (petri-api)        # Axum routes, Swagger UI at /swagger-ui
  nats/          (petri-nats)       # NatsEventPublisher, listeners (injection/removal/update/signal/bridge)
  scheduler-bridge/ (petri-scheduler-bridge) # Reusable infra: SignalPublisher, CheckpointStore, backoff
  nomad/         (petri-nomad)      # NomadClient, NomadWatcher (Nomad event stream → NATS signals)
  slurm/         (petri-slurm)     # SlurmClient, SlurmWatcher (Slurm scheduler integration)
  test-harness/  (petri-test-harness) # Integration test helpers (testcontainers-based)
sdk/             (aithericon-sdk)   # Scenario definition DSL (Context, PlaceHandle, TransitionBuilder)
sdk-derive/      (aithericon-sdk-derive) # Proc macros: #[token], #[step]
cli/                                # CLI tools
```

## Architecture

### Layered Crate Design

Domain → Application → Infrastructure → API → core-engine binary. Dependencies flow inward. The `nats` crate wraps infrastructure stores with NATS publishing.

### Place Kinds

Every place in a Petri net has a specific `kind` that defines how it interacts with the world:

1. **`internal`** — Regular workflow state (default).
2. **`signal`** — Receives external events from adapters.
3. **`bridge_in`** — Receives tokens from other nets.
4. **`bridge_out`** — Forwards tokens to a remote net via NATS (never enters local marking).
5. **`bridge_reply`** — Routes tokens back to the sender's reply address.
6. **`terminal`** — Marks a final state. Token arrival + quiescence triggers `NetCompleted` event and eval loop shutdown.

### Transition Logic

Transitions define the action taken when firing:

1. **`rhai`** — Pure data transformation (default).
2. **`effect`** — Executed by a registered handler for side-effects. Results are stored in the event log for deterministic replay.
3. **`wasm`** — High-performance compiled logic (future).

### Event Sourcing

All state changes produce `DomainEvent` variants (defined in `petri-domain`). Events are appended to an immutable log with SHA256 hash chaining. `PersistedEvent` wraps each event with sequence number, timestamp, and hash. `NatsEventPublisher` decorates `MemoryEventStore` to publish events to NATS JetStream.

### NATS Streaming Model

Single global stream `PETRI_GLOBAL` captures all `petri.>` subjects:

- `petri.events.{net_id}.>` — Authoritative net events (including lifecycle: `net.created`, `net.completed`, `net.cancelled`)
- `petri.bridge.{target_net_id}.{place}` — Cross-net token transfer
- `petri.commands.inject|remove|update.token` — Token manipulation commands
- `petri.commands.create_net` — Programmatic net creation
- `petri.signal.{net_id}.>` — External signals
- `petri.claims.>` — Claim protocol messages

Listeners (in `petri-nats`) consume from these subjects: `TokenInjectionListener`, `TokenRemovalListener`, `TokenUpdateListener`, `SignalListener`, `CrossNetBridge`, `GlobalSignalListener`, `CreateNetListener`.

KV buckets: `KV_NET_METADATA` (lifecycle status, tombstone store), `KV_NET_ACTIVITY` (idle tracking for hibernation).

### Multi-Net Architecture

`NetRegistry` in `petri-api` manages multiple independent net instances, each with its own event store, topology, and marking projection. Both flat (backward-compatible) and net-scoped API routes exist.

### Net Lifecycle & Hibernation

Nets follow a **Wake-Run-Hibernate** lifecycle (ADR-13, 14, 15, 16):

- **Terminal places** (`PlaceKind::Terminal`) trigger `NetCompleted` events when quiescent + token present. The eval loop emits the event, cancels per-net listeners, and stops.
- **Lifecycle events** (`NetCreated`, `NetCompleted`, `NetCancelled`) are hash-chained domain events. `NetMetadataProjection` materializes them into `KV_NET_METADATA` for fast status queries.
- **ActivityTracker** writes per-net timestamps to `KV_NET_ACTIVITY` on each eval cycle and signal delivery.
- **HibernationMaster** watches `KV_NET_ACTIVITY`, spawns sleep tasks per net, and hibernates idle nets (cancel token + remove from registry). Uses double-check on expiry to prevent racing with fresh activity.
- **GlobalSignalListener** replaces per-net signal consumers with a single `petri.signal.>` consumer. Routes signals via `NetResolver`, which checks metadata tombstones before waking nets.
- **CreateNetListener** enables programmatic net creation via `petri.commands.create_net`.

### Evaluation Engine

In `petri-application`: transitions fire by checking Rhai guard expressions, verifying input port cardinality, consuming tokens, executing Rhai logic, and routing outputs. Transitions with more inputs fire first (specificity priority). Effect transitions store deterministic results for replay.

### Schema Validation

`SchemaRegistry` in `petri-application` enforces JSON Schema constraints on token data at runtime. Definitions come from the AIR format's `definitions` map and are compiled into `jsonschema::Validator` instances at scenario load time. Validation occurs at three boundaries:

1. **Output tokens** — After transition script/effect execution, output tokens are validated against port `schema_ref` (skip `_error` port)
2. **Token injection** — Tokens created via `create_token()` or NATS injection are validated against place `token_schema`
3. **Input binding** — During `find_valid_binding()`, tokens failing schema validation are skipped (transition not enabled)

`ExecutionConfig` controls which checks are active (output and injection on by default, input binding opt-in). Effect handlers can declare `port_schemas()` for registration-time contract validation. When no definitions are present, all validation is bypassed.

### Scheduler Integration

`petri-nomad` has two sides: imperative (`NomadClient` dispatches parameterized jobs with routing metadata in meta tags) and reactive (`NomadWatcher` watches Nomad allocation events, publishes `ExternalSignal` to NATS). `petri-scheduler-bridge` extracts reusable infra (signal publishing, checkpoint persistence, backoff/reconnect).

### SDK

The SDK provides a fluent Rust DSL for defining scenarios that compile to AIR (Actor Interface Runtime) JSON format. Key types: `Context`, `PlaceHandle<T>`, `TransitionBuilder`, `ResourceBuilder`. Proc macros `#[token]` (adds derives) and `#[step]` (functional transition syntax with guard support).

## Key Environment Variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `NATS_URL` | `nats://localhost:4333` | NATS server address |
| `NET_ID` | `default` | Net instance identifier |
| `PORT` | `3030` | HTTP API port |
| `SCHEDULER_BACKEND` | `mock` | `mock` or `nomad` |
| `SCHEDULER_JOB_TEMPLATE` | `default` | Nomad parameterized job ID |
| `SCHEDULER_SIGNAL_ROUTES` | — | Status-to-place routing (e.g., `running:sig_running,completed:sig_completed`) |
| `PETRI_VALIDATE_SCHEMAS` | `true` | Set to `false` to disable all schema validation |
| `PETRI_IDLE_TIMEOUT_SECS` | `300` | Idle seconds before a net hibernates (ADR-16) |
| `PETRI_MAX_EVENT_TAIL_BYTES` | `16777216` | Byte cap on the in-memory event tail (16 MiB); prefix folds into a base marking |
| `PETRI_MAX_DEDUP_ENTRIES` | `16384` | Max distinct one-shot `(place,dedup_id)` entries retained for redelivery suppression; FIFO-evicts the oldest, bounding BOTH the in-memory idempotency index AND the snapshot dedup set |
| `PETRI_SNAPSHOT_STORE_ENDPOINT` | — | Object-store endpoint for wake snapshots (ADR-20). **Unset → snapshots disabled** (wake full-replays) |
| `PETRI_SNAPSHOT_STORE_BACKEND` | `s3` | `s3` \| `local` \| `gcs` \| `azblob` \| `sftp` |
| `PETRI_SNAPSHOT_STORE_{BUCKET,REGION,PREFIX,ACCESS_KEY,SECRET_KEY}` | — | Snapshot object-store config (independent of `ARTIFACT_STORE_*`) |
| `PETRI_SNAPSHOT_MAX_BYTES` | `268435456` | Sanity cap on a serialized snapshot (256 MiB) |
| `RUST_LOG` | `info` | Tracing filter (e.g., `info,petri_application=debug`) |

## Debugging Petri Nets with the `mekhan-debug` CLI

**When debugging net behavior, always use the `mekhan-debug` CLI** rather than raw curl or guessing at state. Build with `cargo build -p aithericon-cli`.

```bash
# Overview of all deployed nets
mekhan-debug status

# Scan ALL nets for errors (EffectFailed + ErrorOccurred) in one shot
mekhan-debug errors
mekhan-debug errors --last 50

# Current token marking — where are tokens, which transitions are enabled
mekhan-debug state <net-id>

# Recent events (last 20 by default)
mekhan-debug events <net-id>
mekhan-debug events <net-id> --last 50
mekhan-debug events <net-id> --type EffectFailed

# Live event stream
mekhan-debug events <net-id> --tail

# Cross-net tracing by trace ID or signal key
mekhan-debug trace <trace_id_or_signal_key>

# Interactive: wake, fire, inject
mekhan-debug wake <net-id>
mekhan-debug fire <net-id> <transition-id>
mekhan-debug inject <net-id> <place-id> '{"key": "value"}'
```

When something is stuck: `status` -> `errors` -> `state <net>` -> `trace <key>`.

## Python Job Helpers (SDK)

`aithericon_sdk::python_job` provides typed helpers for dispatching Python executor jobs:

- **`PythonJobConfig`** — Script filename, requirements, virtualenv, SDK, nix, stream events
- **`JobInput::storage_path()`** — Reference a script/artifact in S3 (preferred for Nomad deployments)
- **`JobInput::inline()`** — Embed JSON config values
- **`JobInput::script()`** — Semantic alias for `raw()`, for scripts small enough to travel in tokens
- **`python_job_rhai()`** / `python_job_rhai_with_var()` — Generate Rhai for static job dispatch
- **`python_job_rhai_with_dynamic()`** — Generate Rhai with batch loop for dynamic inputs from `read_input_batch`
- **`PythonJobDispatch::wire()`** — Full wiring in one call (auto-registers `script_content` as Rhai var)

**Important**: When jobs go through Nomad (scheduler relay), use `storage_path` for scripts — Nomad's parameterized job payload has tight size limits. `JobInput::raw()` / `JobInput::script()` embeds content in the token, which can exceed this limit.

## Documentation

Detailed docs live in `/docs` organized by audience:
- `sdk/` — Core concepts, macros, contracts & helpers, ML workflows, Python SDK vision
- `engine/` — AIR format, execution rules, NATS streaming
- `integration/` — Cross-net bridge, claim protocol, adapter guide
- `adr/` — Architecture decision records (07-16)
- `ARCHITECTURE.md` — System design overview (resource-as-state-machine, PlaceKinds, effects)
- `README.md` — Full categorized index with reading order
