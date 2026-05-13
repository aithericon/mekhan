# Aithericon Core Prototype (Lab Edition)

A "Digital Lab" prototype demonstrating Colored Petri Nets with Event Sourcing.

## Architecture

This crate is the Rust engine: domain logic, event sourcing, and REST API. The
visualizer / debugger UI lives in the platform's `app/` (Mekhan); it talks to
this engine over HTTP and NATS.

## Quick Start

```bash
cd core-engine
cargo run
```

The API will be available at `http://localhost:3000` with Swagger UI at `/swagger-ui`.

## API Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/topology` | Returns Petri Net structure |
| GET | `/api/events` | Returns full event log |
| GET | `/api/state` | Returns current token marking |
| POST | `/api/command/fire/{id}` | Fire a transition |
| POST | `/api/command/reset` | Reset event log |

## Concepts

### Colored Petri Nets

- **Places**: Locations that hold tokens. Kinds include `Internal`, `Signal`, `BridgeIn`, `BridgeOut`, and `BridgeReply`.
- **Transitions**: Actions that consume and produce tokens. Can be `Rhai` logic or `Effect` handlers (side-effects).
- **Tokens**: Colored data (Unit, Integer, or structured JSON Data).
- **Arcs**: Connect places to transitions with weights.

### Event Sourcing

All state changes are recorded as immutable events with hash chaining:

- `NetInitialized` - Initial topology
- `TokenCreated` - New token added
- `TransitionFired` - Transition executed
- `TokenConsumed` - Token removed

The frontend can replay these events to visualize state at any point in history.

## Running with Nomad Integration

The engine can dispatch batch jobs to [HashiCorp Nomad](https://www.nomadproject.io/) and track their lifecycle via signals. This requires NATS JetStream for signal delivery.

### Prerequisites

1. **Nomad dev agent** running locally:
   ```bash
   nomad agent -dev
   ```

2. **NATS server** with JetStream enabled:
   ```bash
   nats-server -js
   ```

### 1. Register the Nomad job template

The engine dispatches work by dispatching a parameterized Nomad job. Register the template once:

```bash
curl -X POST http://localhost:4646/v1/jobs -d '{
  "Job": {
    "ID": "default",
    "Name": "default",
    "Type": "batch",
    "Datacenters": ["dc1"],
    "ParameterizedJob": {
      "Payload": "optional",
      "MetaRequired": [],
      "MetaOptional": [
        "petri_net_id", "petri_place", "petri_corr",
        "petri_signal_running", "petri_signal_completed", "petri_signal_failed"
      ]
    },
    "TaskGroups": [{
      "Name": "main",
      "Count": 1,
      "RestartPolicy": { "Attempts": 0, "Mode": "fail" },
      "ReschedulePolicy": { "Attempts": 0 },
      "Tasks": [{
        "Name": "petri-worker",
        "Driver": "raw_exec",
        "Config": { "command": "/bin/echo", "args": ["done"] },
        "Resources": { "CPU": 1, "MemoryMB": 32 }
      }]
    }]
  }
}'
```

Replace `/bin/echo done` with your actual workload. The `MetaOptional` fields are stamped by the engine at dispatch time for signal routing.

### 2. Start the engine with Nomad support

```bash
cd core-engine
SCHEDULER_BACKEND=nomad \
NOMAD_ADDR=http://localhost:4646 \
SCHEDULER_SIGNAL_ROUTES="running:sig_running,completed:sig_completed,failed:sig_failed" \
  cargo run -p core-engine --features nomad
```

### 3. Deploy the nomad batch scenario

```bash
cargo run -p aithericon-sdk --example nomad_batch_net -- --deploy
```

### 4. Evaluate

Trigger evaluation via the engine API (or the Mekhan `app/` debugger). The 3 seed jobs will dispatch to Nomad, the NomadWatcher detects completion via Nomad's event stream, publishes signals to NATS, the SignalListener injects tokens, and subsequent evaluations route them to the `completed` place.

### Environment variables

| Variable | Description | Example |
|----------|-------------|---------|
| `SCHEDULER_BACKEND` | Scheduler type (`nomad`) | `nomad` |
| `NOMAD_ADDR` | Nomad API address | `http://localhost:4646` |
| `SCHEDULER_SIGNAL_ROUTES` | Per-status signal routing (colon-separated `status:place` pairs) | `running:sig_running,completed:sig_completed,failed:sig_failed` |
| `SCHEDULER_JOB_TEMPLATE` | Nomad parameterized job ID to dispatch (default: `default`) | `my-batch-worker` |
| `NATS_URL` | NATS server URL (default: `nats://localhost:4333`) | `nats://localhost:4333` |

## Sample Scenario: Resource Allocation

The default scenario models a worker-task assignment system:

```
[Workers: 3] ──┐
               ├──▶ (Assign) ──▶ [In Progress] ──▶ (Complete) ──┬──▶ [Completed]
[Tasks: 5] ────┘                                                │
       ▲                                                        │
       └────────────────────────────────────────────────────────┘
                            (worker returned)
```
