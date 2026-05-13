# ML & Scientific Workflow Patterns

> How Petri-Lab's primitives compose into iterative, fault-tolerant scientific computing workflows — with a concrete case study in gradient-free optimization of physics simulations.

## Table of Contents

1. [Motivation](#motivation)
2. [Building Blocks](#building-blocks)
3. [Pattern: Black-Box Optimization Loop](#pattern-black-box-optimization-loop)
4. [State Recovery Strategies](#state-recovery-strategies)
5. [Composable Workflow Nets](#composable-workflow-nets)
6. [Example: Micromagnetics + Nevergrad](#example-micromagnetics--nevergrad)
7. [Resumability](#resumability)

---

## Motivation

Scientific computing workflows differ from typical business orchestration in several important ways:

- **Expensive evaluations.** A single micromagnetics simulation (mumax3, OOMMF) or CFD solve can run for hours on GPU clusters. Losing progress is costly.
- **Iterative refinement.** Optimization loops, active learning, and surrogate modelling require cycles — propose parameters, evaluate, update model, repeat.
- **Parallel evaluation.** Multiple candidate parameter sets should run concurrently to maximize throughput on cluster resources.
- **Heterogeneous compute.** Steps mix Python (ML training, acquisition functions), compiled solvers (physics codes), and data processing — often on different hardware.
- **Resumability.** Multi-day campaigns must survive crashes, preemptions, and restarts without losing work.

### Why DAG Orchestrators Fall Short

Traditional workflow engines (Airflow, Argo, Prefect) model workflows as **directed acyclic graphs**. This creates a fundamental problem for iterative workflows: the optimization loop cannot live inside the workflow. Instead, a separate optimizer process must schedule new DAG runs per iteration, managing its own state outside the orchestration layer.

This leads to:
- **Fragile external state.** The optimizer process holds state in memory. If it crashes, the optimization run is lost.
- **Disconnected recovery.** The DAG engine can retry individual tasks, but cannot recover the optimizer's iteration state.
- **Scattered coordination.** Multiple independent watchers (scheduler events, task status, timeouts) each with their own retry logic, creating race conditions.

### Why Petri Nets Fit

Colored Petri nets address these problems directly:

- **Cycles are native.** A back-edge in the net topology IS a loop. No external process needed.
- **Tokens carry state.** The optimizer's state (or a reference to it) is a token flowing through the net, subject to the same event sourcing as everything else.
- **Synchronization is structural.** Fan-in (waiting for parallel results) is a multi-input transition with a guard. No polling, no external counters.
- **Event sourcing = resumability.** Every token movement is a hash-chained event. Crash and restart replays to exact state.

---

## Building Blocks

The following primitives, already present in Petri-Lab, compose into scientific workflow patterns without special-case code.

### Reference Tokens

Scientific workflows produce large artifacts — magnetization fields, model checkpoints, training datasets. These don't belong inside token payloads. Instead, use **reference tokens**: lightweight JSON tokens carrying URIs to data in object storage.

```rust
#[token]
struct SimulationResult {
    candidate_id: String,
    loss: f64,
    artifact_uri: String,       // e.g., "s3://bucket/sim-results/run-042.h5"
    metadata: serde_json::Value, // dimensions, convergence info, etc.
}
```

The executor's `ArtifactStore` (with OpenDAL backends for S3/GCS/Azure) handles upload during job execution. The result token carries only the reference — the net routes it like any other token.

### Effect Transitions and the Executor

Effect transitions delegate work to external systems. For scientific workflows, the primary target is the **aithericon-executor** — a distributed task executor with pluggable backends:

- **PythonBackend** — Runs Python scripts in isolated virtualenvs (PyTorch training, Nevergrad optimization, simulation post-processing)
- **DockerBackend** — Runs containerized workloads (mumax3 with CUDA, domain-specific solvers)
- **ProcessBackend** — Fork+exec for compiled binaries

The executor provides:
- **Input staging** — Files and parameters written to a run directory before execution
- **IPC sidecar** — Child processes report progress, artifacts, and metrics mid-execution via gRPC
- **Output collection** — Results and artifacts gathered after completion
- **Metadata pass-through** — Routing metadata (which net, which place, which correlation key) echoed in all status messages

### Signal-Driven Feedback

When a simulation completes on a cluster, the result flows back through a reactive pipeline:

```
Executor completes job
  → StatusReporter publishes to NATS (executor.status.{id}.completed)
  → ExecutorWatcher extracts routing metadata, publishes ExternalSignal
  → SignalListener injects token into target signal place
  → Downstream transition fires, consuming the result
```

This is the same signal pathway used for Nomad and Slurm integration. The net doesn't know or care which scheduler ran the job — it sees a result token arrive in a signal place.

### Cross-Net Bridges

Reusable workflow components are independent nets connected via bridges. A bridge-out place forwards tokens to a remote net's bridge-in place via NATS. See [Cross-Net Bridge](../integration/cross-net-bridge.md) for the full specification.

For scientific workflows, this enables separation of concerns:
- An **optimizer net** handles the ask/tell loop
- A **simulation runner net** handles job dispatch and result collection
- A **campaign net** composes them for a specific study

Each is independently deployable, testable, and reusable.

### Batch Input Ports

A port with `Batch` cardinality consumes **all available tokens** (up to arc weight) from a place in a single firing. This is critical for the optimization pattern: the optimizer transition should drain all pending results, not process them one at a time.

```rust
ctx.transition("optimize", "Run Optimization Cycle")
    .auto_input_batch("results", &result_place)  // Drain all available results
    .auto_input("state", &optimizer_state)        // Single optimizer reference
    .auto_output("state", &optimizer_state)       // Updated optimizer reference
    .auto_output("candidates", &candidate_place)  // New candidates to evaluate
    .effect("nevergrad_handler");
```

---

## Pattern: Black-Box Optimization Loop

The core pattern for gradient-free optimization (Nevergrad, Optuna, custom algorithms) maps directly to a Petri net cycle.

### The Ask/Tell Interface

Most black-box optimizers expose a simple interface:

```python
optimizer = ng.optimizers.CMA(parametrization=param, budget=100)

# Ask: propose candidate parameter sets
candidates = [optimizer.ask() for _ in range(batch_size)]

# ... evaluate candidates (expensive simulations) ...

# Tell: report results back
for candidate, loss in zip(candidates, losses):
    optimizer.tell(candidate, loss)
```

This maps to a single transition that wakes up, processes available results, and proposes new candidates.

### Net Topology

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     OPTIMIZATION LOOP (single net)                          │
│                                                                             │
│                                                                             │
│  ┌──────────────┐     ┌─────────────────┐     ┌──────────────────────┐    │
│  │  results     │────►│                 │────►│  candidates          │    │
│  │  (batch in)  │     │    optimize     │     │  (fan-out to sims)   │    │
│  └──────────────┘     │                 │     └──────────────────────┘    │
│        ▲              │  - drain results│                │               │
│        │              │  - tell() each  │                ▼               │
│        │              │  - check budget │         ┌──────────────┐      │
│        │              │  - ask() next   │         │  dispatch    │      │
│        │              │  - serialize    │         │  (bridge out │      │
│        │              │                 │         │   to sims)   │      │
│        │              └────────┬────────┘         └──────────────┘      │
│        │                       │                                         │
│        │              ┌────────┴────────┐                                │
│        │              │  optimizer_ref  │  ← single token = natural     │
│        │              │  (loop back)    │    mutex on optimizer state    │
│        │              └─────────────────┘                                │
│        │                                                                 │
│        └──────────────── results arrive from simulation signals ─────────│
│                                                                             │
│  Convergence: guard on back-edge checks budget/loss threshold.             │
│  When done, optimizer_ref routes to terminal place instead.                │
└─────────────────────────────────────────────────────────────────────────────┘
```

### The Hybrid Wake Model

The optimizer transition uses a **hybrid wake/drain/serialize** model:

1. **Wake.** The transition is enabled when at least one result token is in the results place AND the optimizer_ref token is available.
2. **Drain.** The batch input port consumes ALL pending results in one firing.
3. **Tell.** The executor job loads the serialized optimizer from storage, calls `tell()` for each result.
4. **Ask.** If budget remains and convergence is not met, calls `ask()` to propose new candidates.
5. **Serialize.** Writes the updated optimizer state back to storage.
6. **Sleep.** Emits the updated optimizer_ref token (releasing the "lock") and candidate tokens.

This design has several properties:

**Natural concurrency safety.** There is only one `optimizer_ref` token. When consumed by the optimize transition, no other firing can touch the optimizer state. Results that arrive while the optimizer is running accumulate in the results place and are drained on the next cycle.

**No straggler blocking.** Unlike batch-synchronous designs that wait for all N simulations to complete, this processes whatever results are available. Fast simulations contribute immediately; slow ones are picked up in the next cycle.

**Algorithm-agnostic.** Whether the underlying optimizer is CMA-ES (wants batches), OnePlusOne (processes one at a time), or a custom surrogate model, the net topology is the same. The optimizer job handles algorithm-specific logic.

### Token Types

```rust
/// Serialized optimizer state — carries a reference to the pickled object in storage.
#[token]
struct OptimizerState {
    campaign_id: String,
    iteration: i64,
    total_evaluations: i64,
    budget: i64,
    best_loss: f64,
    artifact_uri: String,           // "s3://bucket/optimizer/campaign-001/state.pkl"
    converged: bool,
}

/// A candidate parameter set proposed by the optimizer.
#[token]
struct Candidate {
    campaign_id: String,
    candidate_id: String,
    iteration: i64,
    parameters: serde_json::Value,  // Algorithm-specific parameter vector
}

/// Result from evaluating a candidate (simulation output).
#[token]
struct EvaluationResult {
    campaign_id: String,
    candidate_id: String,
    iteration: i64,
    loss: f64,
    artifact_uri: String,           // Reference to full simulation output
    metadata: serde_json::Value,    // Convergence info, timing, etc.
}
```

### SDK Sketch

```rust
fn definition(ctx: &mut Context) {
    // -- Places --
    let optimizer_state = ctx.state::<OptimizerState>("optimizer_state", "Optimizer State");
    let results = ctx.state::<EvaluationResult>("results", "Evaluation Results");
    let candidates = ctx.state::<Candidate>("candidates", "Candidates");
    let completed = ctx.state::<OptimizerState>("completed", "Optimization Complete");

    // Bridge out — dispatch candidates to simulation runner net
    let to_simulations = ctx.bridge_out::<Candidate>(
        "to_simulations", "To Simulations",
        "simulation-runner", "candidate_inbox",
    );

    // Bridge in — receive results from simulation runner net
    let result_inbox = ctx.bridge_in::<EvaluationResult>(
        "result_inbox", "Result Inbox",
    );

    // -- Seed initial optimizer state --
    ctx.seed_one(&optimizer_state, OptimizerState {
        campaign_id: "campaign-001".into(),
        iteration: 0,
        total_evaluations: 0,
        budget: 100,
        best_loss: f64::MAX,
        artifact_uri: "".into(),    // No artifact yet — first cycle initializes
        converged: false,
    });

    // -- Route incoming results to the results place --
    ctx.transition("collect_result", "Collect Result")
        .auto_input("result", &result_inbox)
        .auto_output("out", &results)
        .logic(r#"#{ out: result }"#);

    // -- Core optimization cycle (effect transition) --
    ctx.transition("optimize", "Run Optimization Cycle")
        .auto_input_batch("results", &results)
        .auto_input("state", &optimizer_state)
        .guard(r#"state.converged == false && state.total_evaluations < state.budget"#)
        .auto_output("state", &optimizer_state)
        .auto_output("candidates", &candidates)
        .effect("nevergrad_handler");

    // -- Fan out: dispatch each candidate to simulation --
    ctx.transition("dispatch_candidate", "Dispatch Candidate")
        .auto_input("candidate", &candidates)
        .auto_output("out", &to_simulations)
        .logic(r#"#{ out: candidate }"#);

    // -- Convergence: route to terminal when done --
    ctx.transition("finish", "Optimization Complete")
        .auto_input("state", &optimizer_state)
        .guard(r#"state.converged == true || state.total_evaluations >= state.budget"#)
        .auto_output("done", &completed)
        .logic(r#"#{ done: state }"#);
}
```

---

## State Recovery Strategies

The optimizer holds internal state (covariance matrices for CMA-ES, population vectors for evolutionary algorithms, surrogate model weights). How this state survives across wake cycles and crashes is an architectural choice.

### Option A: Artifact Serialization

After each wake cycle, the optimizer job serializes the optimizer object (e.g., via Python `pickle`) and uploads it to object storage. The `optimizer_ref` token carries the artifact URI. On the next cycle, the job downloads and deserializes the object.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  ARTIFACT SERIALIZATION CYCLE                                               │
│                                                                             │
│  1. Load:   download s3://bucket/optimizer/state.pkl                       │
│  2. Tell:   optimizer.tell(candidate, loss)  for each pending result       │
│  3. Ask:    candidates = [optimizer.ask() for _ in range(batch)]           │
│  4. Save:   upload updated state.pkl to s3                                 │
│  5. Emit:   updated optimizer_ref token + candidate tokens                 │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Advantages:**
- Exact state preservation — internal bookkeeping, RNG state, everything.
- Works for all algorithms without qualification.
- Fast wake-up: deserialize is O(1), not O(n) in history length.

**Risks:**
- Pickle compatibility across Python/Nevergrad versions. A library upgrade can break deserialization.
- Opaque blob — the optimizer state is not human-inspectable.
- Storage management — artifact accumulates per iteration.

### Option B: Event Replay

No artifact is stored. The optimizer is reconstructed from scratch each cycle by replaying the full tell history — every (candidate, loss) pair from the event log.

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  EVENT REPLAY CYCLE                                                         │
│                                                                             │
│  1. Init:   optimizer = ng.optimizers.CMA(parametrization, budget)         │
│  2. Replay: for (candidate, loss) in history:                              │
│                 c = optimizer.parametrization.spawn_child(candidate)        │
│                 optimizer.tell(c, loss)                                     │
│  3. Ask:    candidates = [optimizer.ask() for _ in range(batch)]           │
│  4. Emit:   candidate tokens (no artifact to save)                         │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Advantages:**
- No artifact to manage. No serialization format, no pickle compatibility.
- The event log IS the state — fully transparent, inspectable, replayable.
- Library upgrades are safe: replay uses current API, not stored binary format.

**Risks:**
- Algorithm fidelity. Some optimizers maintain internal state that isn't fully determined by the tell history (step-size adaptation, internal counters, RNG sequences). Replayed state may diverge from the original.
- O(n) replay cost grows with iteration count. For long campaigns, this adds latency per cycle.

### Comparison

| Aspect | Artifact Serialization | Event Replay |
|--------|----------------------|--------------|
| **Fidelity** | Exact | Algorithm-dependent |
| **Wake-up cost** | O(1) deserialize | O(n) replay all tells |
| **Library upgrades** | Breaking (pickle compat) | Safe (current API) |
| **Transparency** | Opaque binary blob | Full history in events |
| **Storage** | One artifact per iteration | No extra storage |
| **Complexity** | Artifact upload/download | History query |

### Recommended Approach

For algorithms where replay fidelity is confirmed (test empirically: serialize vs. replay, compare `ask()` output), use **event replay**. It's cleaner and avoids the pickle compatibility risk.

For algorithms where replay diverges, or for campaigns exceeding hundreds of iterations (where replay latency matters), use **artifact serialization**.

Both approaches are compatible with the same net topology. The difference is internal to the executor job — the optimizer_ref token either carries an artifact URI or enough metadata to query the event history.

### How to Test Fidelity

```python
import nevergrad as ng
import pickle

param = ng.p.Array(shape=(5,)).set_bounds(-1, 1)
opt_a = ng.optimizers.CMA(parametrization=param, budget=200)

history = []
for _ in range(50):
    c = opt_a.ask()
    loss = sum(c.value ** 2)
    opt_a.tell(c, loss)
    history.append((c.value.tolist(), loss))

# Serialize
state_a = pickle.dumps(opt_a)

# Replay from history
opt_b = ng.optimizers.CMA(parametrization=param, budget=200)
for params, loss in history:
    c = opt_b.parametrization.spawn_child().set_standardized_data(params)
    opt_b.tell(c, loss)

# Compare: do they propose the same next candidate?
next_a = pickle.loads(state_a).ask().value
next_b = opt_b.ask().value
print(f"Match: {np.allclose(next_a, next_b)}")
```

Run this for each target algorithm. If `ask()` outputs match, event replay is faithful.

---

## Composable Workflow Nets

The optimization loop and the simulation execution are separate concerns that compose through bridges.

### Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                       COMPOSABLE SCIENTIFIC WORKFLOW                        │
│                                                                             │
│  ┌─────────────────────────┐   bridge    ┌──────────────────────────────┐  │
│  │                         │─────────────►│                              │  │
│  │    OPTIMIZER NET        │  candidates  │    SIMULATION RUNNER NET     │  │
│  │                         │              │                              │  │
│  │  - ask/tell loop        │   results    │  - dispatch to executor      │  │
│  │  - convergence check    │◄─────────────│  - collect results           │  │
│  │  - state management     │   bridge     │  - handle retries            │  │
│  │                         │              │                              │  │
│  └─────────────────────────┘              └──────────┬───────────────────┘  │
│                                                      │                      │
│                                                      │ executor jobs        │
│                                                      ▼                      │
│                                           ┌──────────────────────┐          │
│                                           │  AITHERICON EXECUTOR │          │
│                                           │                      │          │
│                                           │  Python / Docker     │          │
│                                           │  (mumax3, PyTorch,   │          │
│                                           │   post-processing)   │          │
│                                           └──────────────────────┘          │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Optimizer Net

Generic. Parameterized by:
- Algorithm choice (CMA-ES, OnePlusOne, NGOpt, custom)
- Parameter space definition (bounds, types, constraints)
- Budget (max evaluations) and convergence criteria

Knows nothing about what is being optimized. Receives `EvaluationResult` tokens with a `loss` field, emits `Candidate` tokens with a `parameters` field. Reusable across any optimization target.

### Simulation Runner Net

Generic. Parameterized by:
- Execution spec (Docker image, Python script, resource requirements)
- Input/output mapping (how candidate parameters become simulation inputs, how outputs become loss values)
- Retry policy (max retries, backoff)

Knows nothing about the optimization strategy. Receives `Candidate` tokens, dispatches them as executor jobs, collects results, handles failures. Reusable across different simulation codes.

### Campaign Net

Specific to a study. Composes optimizer and simulation runner nets for a particular scientific question. Defines:
- Which optimizer net to use and with what configuration
- Which simulation runner net to use and with what execution spec
- How to seed the initial state
- What to do with the final result

### Reuse Patterns

| Swap | Keep | Result |
|------|------|--------|
| Simulation runner (mumax3 → OOMMF) | Optimizer net | Same optimization, different physics code |
| Optimizer (CMA-ES → Bayesian) | Simulation runner | Same simulation, different search strategy |
| Both | Campaign structure | Different study, same orchestration pattern |

---

## Example: Micromagnetics + Nevergrad

A concrete campaign optimizing magnetic device geometry using mumax3 simulations guided by Nevergrad's CMA-ES optimizer.

### Objective

Find the geometry parameters (width, thickness, spacing) of a magnetic nanostructure that minimize switching field while maintaining thermal stability. Each evaluation requires a full micromagnetics simulation (~10-60 minutes on GPU).

### Token Types

```rust
/// Optimizer state for the micromagnetics campaign.
#[token]
struct MicromagnetsOptimizer {
    campaign_id: String,
    iteration: i64,
    evaluations: i64,
    budget: i64,
    best_loss: f64,
    best_params: serde_json::Value,
    optimizer_uri: String,          // Serialized Nevergrad state
    converged: bool,
}

/// Candidate geometry to simulate.
#[token]
struct GeometryCandidate {
    campaign_id: String,
    candidate_id: String,
    iteration: i64,
    width_nm: f64,
    thickness_nm: f64,
    spacing_nm: f64,
    material: String,
}

/// Simulation result from mumax3.
#[token]
struct SimulationResult {
    campaign_id: String,
    candidate_id: String,
    switching_field_mT: f64,
    thermal_stability: f64,
    loss: f64,                      // Composite objective
    magnetization_uri: String,      // "s3://results/mag-field-042.ovf"
    log_uri: String,                // "s3://results/mumax-042.log"
}
```

### Full Topology

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  OPTIMIZER NET (net-id: "magnetics-optimizer")                              │
│                                                                             │
│  [optimizer_state]─┐                                                       │
│                    ├──►(optimize)──┬──►[optimizer_state] ←── loop back     │
│  [results (batch)]─┘   (effect)   └──►[candidates]                        │
│                                           │                                │
│                                    (dispatch_candidate)                    │
│                                           │                                │
│                                    [to_sims: bridge_out] ──────────────►  │
│                                                                             │
│  ◄──────────────── [result_inbox: bridge_in]                               │
│                           │                                                │
│                    (collect_result)                                         │
│                           │                                                │
│                    [results] ──────────────── feeds back to optimize       │
│                                                                             │
│  [optimizer_state]──►(finish)──►[completed]  (when converged/budget)       │
└─────────────────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────────────────┐
│  SIMULATION RUNNER NET (net-id: "mumax-runner")                             │
│                                                                             │
│  [candidate_inbox: bridge_in]                                              │
│         │                                                                   │
│  (prepare_simulation)  ← generates mumax3 input script from parameters     │
│         │                                                                   │
│  [job_pending]                                                              │
│         │                                                                   │
│  (submit_job)  ← effect: dispatch to executor (Docker + mumax3 image)      │
│         │                                                                   │
│  [job_running] + [sig_status: signal]                                      │
│         │                                                                   │
│  (handle_completed) ← guard: signal.status == "completed"                  │
│         │                                                                   │
│  (parse_results)  ← effect: extract switching field, stability from output │
│         │                                                                   │
│  [to_optimizer: bridge_out] ──────────────────────────────────────────────► │
│                                                                             │
│  (handle_failed) + retry logic                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Executor Job Spec

The mumax3 simulation runs as a Docker container via the executor:

```python
# Executed by PythonBackend in the executor
import json
import subprocess

# Read staged input (parameters from candidate token)
with open("/aithericon/inputs/parameters.json") as f:
    params = json.load(f)

# Generate mumax3 input script
script = f"""
SetGridSize(128, 128, 1)
SetCellSize({params['width_nm']}e-9/128, {params['width_nm']}e-9/128, {params['thickness_nm']}e-9)
Msat  = 800e3
Aex   = 13e-12
alpha = 0.02
m = Uniform(1, 0, 0)
// ... geometry and field sweep configuration ...
"""

with open("/aithericon/run/simulation.mx3", "w") as f:
    f.write(script)

# Run mumax3
result = subprocess.run(
    ["mumax3", "/aithericon/run/simulation.mx3"],
    capture_output=True, timeout=3600
)

# Parse results from mumax3 output table
# ... extract switching_field, thermal_stability ...

# Write output for collection
output = {
    "switching_field_mT": switching_field,
    "thermal_stability": stability,
    "loss": switching_field - 0.5 * stability,  # composite objective
}
with open("/aithericon/outputs/result.json", "w") as f:
    json.dump(output, f)
```

---

## Resumability

This is the property that motivated the migration from the legacy platform.

### What Happens on Crash

Consider an optimization campaign 40 iterations in, with 3 simulations running on a GPU cluster. The engine process crashes.

**With the legacy DAG orchestrator:** The optimizer process is dead. Its internal state (40 iterations of CMA-ES covariance updates) is lost. The 3 running simulations will complete and report results to a dead listener. The entire campaign must restart from iteration 0.

**With Petri-Lab:**

1. **Engine restarts.** The event store replays all events. The marking is reconstructed: `optimizer_state` place holds the token from iteration 40, `results` place holds any results that arrived before the crash.

2. **Running simulations continue.** They are external processes (executor jobs on the cluster). They report status via NATS. The `ExecutorWatcher` resumes from its checkpoint, picks up status messages, and injects result tokens into the net.

3. **Optimizer state is intact.** Either:
   - (Artifact mode) The serialized optimizer is in object storage. The token carries the URI. Next cycle loads it.
   - (Replay mode) The tell history is in the event log. Next cycle reconstructs from it.

4. **No lost work.** Results from simulations that completed during downtime are in NATS (JetStream persists messages). The `SignalListener` advances its epoch to skip stale signals from previous scenario loads, but checkpoint-based recovery preserves in-flight results.

### Recovery Guarantees

| Component | Recovery Mechanism |
|-----------|-------------------|
| **Net marking** | Event sourcing replay (hash-chained log) |
| **Optimizer state** | Artifact in storage or event replay |
| **Running simulations** | External processes continue; watcher resumes from checkpoint |
| **In-flight results** | NATS JetStream persistence; SignalListener injects on reconnect |
| **Correlation** | Candidate IDs in tokens match results to iterations |
| **Deduplication** | JetStream `Nats-Msg-Id` prevents double-injection on watcher restart |

The net resumes from exactly where it was. No manual intervention, no restart-from-zero, no lost simulation time.

---

## Further Reading

- [Core Concepts](./core-concepts.md) — Petri net fundamentals
- [Cross-Net Bridge](../integration/cross-net-bridge.md) — Bridge specification and examples
- [Architecture](../ARCHITECTURE.md) — Resource-as-state-machine pattern
- [Execution Rules](../engine/execution-rules.md) — Transition firing, priority, guards
- [Streaming](../engine/streaming.md) — NATS subject hierarchy
