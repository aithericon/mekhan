# Python SDK Vision

This document outlines the architecture for the Python SDK that exposes Petri-Lab to end users without requiring them to understand Petri nets.

## Design Principles

1. **Users don't need to know about Petri nets** — They compose computational building blocks
2. **Fault tolerance is built-in** — Building blocks handle retries, timeouts, failures automatically
3. **Single codebase** — Python SDK wraps the Rust SDK via PyO3, no duplication
4. **User vs Admin separation** — Admins configure infrastructure, users just use it
5. **Functional composition** — Building blocks compose like functions, not imperative wiring

---

## Three-Layer Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    USER LAYER (Python SDK)                               │
│                                                                          │
│   from petri_lab import Workflow, execute, transfer, branch, loop       │
│                                                                          │
│   with Workflow("my-experiment") as wf:                                  │
│       data = wf.input("data")                                           │
│       result = execute("mumax3", script=data, params={...})             │
│       processed = execute("postprocess", input=result.files)            │
│       wf.output("result", processed)                                    │
│                                                                          │
└──────────────────────────────┬──────────────────────────────────────────┘
                               │
                               │ Compiles to
                               ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                 BUILDING BLOCKS LAYER (Rust Components)                  │
│                                                                          │
│   ExecuteNode      - Fault-tolerant job execution with retries          │
│   DataTransfer     - Reliable file transfer with checksums              │
│   HttpRequest      - HTTP with retries, backoff, circuit breaker        │
│   Branch           - Conditional routing                                 │
│   Loop             - Iteration with termination conditions              │
│   Parallel         - Fan-out / fan-in                                   │
│   Checkpoint       - State snapshots for recovery                        │
│   Timeout          - SLA enforcement                                     │
│   Optimizer        - Ask/tell pattern with convergence detection         │
│                                                                          │
└──────────────────────────────┬──────────────────────────────────────────┘
                               │
                               │ Implemented as
                               ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                    RAW PETRI NET LAYER (Rust SDK)                        │
│                                                                          │
│   Context, PlaceHandle, Transition, Token, Arc, Guard, etc.             │
│   (Only used by platform developers, never exposed to users)            │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Building Blocks

### 1. Execute (Run a Node on Scheduler)

```python
from petri_lab import Workflow, execute

with Workflow("simulation") as wf:
    script = wf.input("script", type="file")
    params = wf.input("params", type="json")

    # Execute a registered node template
    # All the Petri net complexity is hidden:
    # - Queuing, scheduling, retries, timeouts, failure handling
    result = execute(
        "mumax3",                    # Node template (admin-defined)
        script=script,
        params=params,

        # Optional overrides (within admin-allowed limits)
        timeout="2h",
        retries=3,
    )

    # result.files, result.logs, result.metrics are available
    wf.output("output", result.files)
```

**Under the hood**, `execute()` expands to a fault-tolerant Petri net subnet:

```
                    ┌─────────────┐
                    │   pending   │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │  t_submit   │──────────▶ Scheduler Adapter
                    └──────┬──────┘
                           │
              ┌────────────┴────────────┐
              ▼                         ▼
       ┌─────────────┐          ┌─────────────┐
       │   running   │          │   queued    │
       │  (claimed)  │          │             │
       └──────┬──────┘          └──────┬──────┘
              │                        │
    ┌─────────┼─────────┐             │
    ▼         ▼         ▼             │
┌───────┐ ┌───────┐ ┌───────┐        │
│ done  │ │failed │ │timeout│◀───────┘
└───────┘ └───┬───┘ └───┬───┘
              │         │
              └────┬────┘
                   ▼
            ┌─────────────┐
            │  t_retry?   │──────▶ (back to pending if retries left)
            └─────────────┘
```

Users never see this. They just call `execute()`.

---

### 2. Transfer (Move Data Between Locations)

```python
from petri_lab import Workflow, execute, transfer

with Workflow("distributed-pipeline") as wf:
    local_data = wf.input("data")

    # Transfer to cluster storage before execution
    remote_data = transfer(
        local_data,
        to="scratch",              # Named storage location
        checksum=True,
    )

    # Execute on cluster (data already there)
    result = execute("process", input=remote_data)

    # Transfer results back
    local_result = transfer(
        result.files,
        to="local",
    )

    wf.output("result", local_result)
```

---

### 3. Branch (Conditional Routing)

```python
from petri_lab import Workflow, execute, branch

with Workflow("conditional-pipeline") as wf:
    data = wf.input("data")
    mode = wf.input("mode")  # "fast" or "full"

    # Branch based on condition
    with branch(mode) as b:

        @b.case("fast")
        def fast_path():
            return execute("quick-process", input=data)

        @b.case("full")
        def full_path():
            step1 = execute("preprocess", input=data)
            step2 = execute("full-process", input=step1.output)
            return execute("postprocess", input=step2.output)

        @b.default
        def fallback():
            return execute("default-process", input=data)

    # b.result contains whichever branch executed
    wf.output("result", b.result)
```

---

### 4. Loop (Iteration)

```python
from petri_lab import Workflow, execute, loop

with Workflow("iterative-optimization") as wf:
    initial_params = wf.input("params")
    target_loss = wf.input("target", default=0.01)

    # Loop until condition met
    with loop(max_iterations=100) as lp:
        # Access current iteration state
        params = lp.state("params", initial=initial_params)

        # Execute simulation
        result = execute("simulate", params=params)
        loss = execute("compute-loss", input=result.output)

        # Update state for next iteration
        new_params = execute("optimizer-step",
            params=params,
            loss=loss.value
        )
        lp.update("params", new_params.output)

        # Termination condition
        lp.stop_when(loss.value < target_loss)

    # lp.final_state contains the last iteration's state
    wf.output("best_params", lp.final_state["params"])
    wf.output("iterations", lp.iteration_count)
```

---

### 5. Parallel (Fan-out / Fan-in)

```python
from petri_lab import Workflow, execute, parallel

with Workflow("parameter-sweep") as wf:
    script = wf.input("script")
    param_grid = wf.input("params")  # List of param dicts

    # Fan-out: execute in parallel for each param set
    with parallel(param_grid) as p:
        result = execute("simulate",
            script=script,
            params=p.item,  # Current item from the list
        )
        p.collect(result.output)

    # Fan-in: aggregate all results
    best = execute("select-best", results=p.results)

    wf.output("best", best.output)
    wf.output("all_results", p.results)
```

---

### 6. Http (External API Calls)

```python
from petri_lab import Workflow, http

with Workflow("api-integration") as wf:
    data = wf.input("data")

    # HTTP request with built-in retries, backoff, timeout
    response = http.post(
        "https://api.example.com/process",
        json=data,
        retries=3,
        timeout="30s",
        backoff="exponential",
    )

    # Handle response
    with branch(response.status_code) as b:
        @b.case(200)
        def success():
            return response.json

        @b.case(429)  # Rate limited
        def rate_limited():
            # Built-in will retry with backoff
            pass

        @b.default
        def error():
            raise WorkflowError(f"API error: {response.status_code}")

    wf.output("result", b.result)
```

---

### 7. Checkpoint (Recovery Points)

```python
from petri_lab import Workflow, execute, checkpoint

with Workflow("long-running") as wf:
    data = wf.input("data")

    step1 = execute("preprocess", input=data)

    # Checkpoint after expensive step
    # If workflow fails after this, it resumes from here
    checkpoint("after-preprocess", state=step1.output)

    step2 = execute("train", input=step1.output)  # Long-running

    checkpoint("after-training", state=step2.output)

    step3 = execute("evaluate", model=step2.output)

    wf.output("result", step3.output)
```

---

### 8. Timeout (SLA Enforcement)

```python
from petri_lab import Workflow, execute, timeout

with Workflow("sla-enforced") as wf:
    data = wf.input("data")

    # Enforce maximum execution time
    with timeout("1h") as t:
        result = execute("long-process", input=data)

    # Handle timeout vs success
    if t.timed_out:
        # Compensation logic
        notify = execute("send-alert", message="SLA breach")
        wf.output("status", "timeout")
    else:
        wf.output("result", result.output)
        wf.output("status", "success")
```

---

### 9. Optimizer (Ask/Tell Pattern)

The `optimizer` building block implements the ask/tell pattern common in black-box optimization
(Nevergrad, Optuna, etc.). It manages optimizer state and coordinates the optimization loop.

```python
from petri_lab import Workflow, execute, optimizer, parallel

with Workflow("parameter-optimization") as wf:
    script = wf.input("script", type="file")
    reference = wf.input("reference", type="file")

    # Create an optimizer with ask/tell interface
    opt = optimizer(
        algorithm="CMA",               # or "NGOpt", "TwoPointsDE", "PSO", etc.
        parameters={
            "Aex": {"range": [1e-12, 50e-12], "scale": "log"},
            "alpha": {"range": [0.001, 0.5], "scale": "linear"},
            "Ku1": {"range": [1e3, 1e6], "scale": "log"},
        },
        budget=500,                    # Total evaluation budget
        batch_size=10,                 # Candidates per ask

        # Optional convergence criteria
        convergence={
            "min_delta": 1e-6,         # Stop if improvement < delta
            "patience": 50,            # Stop after N iterations without improvement
        },
    )

    # The optimization loop - ask/evaluate/tell
    with opt.loop() as lp:
        # ASK: Get batch of candidate parameters
        candidates = lp.ask()

        # EVALUATE: Run simulations in parallel
        with parallel(candidates) as p:
            sim = execute("mumax3", script=script, params=p.item)
            loss = execute("compute-loss",
                simulated=sim.output,
                reference=reference
            )
            p.collect({"params": p.item, "loss": loss.value})

        # TELL: Report results to optimizer
        lp.tell(p.results)

    # Results available after loop
    wf.output("best_params", opt.best_params)
    wf.output("best_loss", opt.best_loss)
    wf.output("history", opt.history)
```

**Under the hood**, the optimizer building block expands to:

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        Optimizer Adapter                                 │
│  ┌─────────────┐                                                        │
│  │  opt_state  │◀───────────────────────────────────────┐               │
│  │  (shared)   │                                        │               │
│  └──────┬──────┘                                        │               │
│         │                                               │               │
│  ┌──────▼──────┐                                        │               │
│  │   t_ask     │                                        │               │
│  │  (export)   │───▶ Optimizer Service                  │               │
│  └──────┬──────┘          │                             │               │
│         │                 │                             │               │
│  ┌──────▼──────┐   ┌──────▼──────┐                     │               │
│  │  pending    │   │ sig_candidates                    │               │
│  │ candidates  │◀──│  (signal)   │                     │               │
│  └──────┬──────┘   └─────────────┘                     │               │
└─────────┼───────────────────────────────────────────────┼───────────────┘
          │                                               │
          ▼                                               │
   ┌─────────────┐                                        │
   │  Parallel   │                                        │
   │ Evaluation  │                                        │
   │  Subnet     │                                        │
   └──────┬──────┘                                        │
          │                                               │
          ▼                                               │
┌─────────────────────────────────────────────────────────┼───────────────┐
│  ┌─────────────┐                                        │               │
│  │  results    │                                        │               │
│  └──────┬──────┘                                        │               │
│         │                                               │               │
│  ┌──────▼──────┐                                        │               │
│  │   t_tell    │                                        │               │
│  │  (export)   │───▶ Optimizer Service ─────────────────┘               │
│  └──────┬──────┘          │                                             │
│         │                 │                                             │
│         │          ┌──────▼──────┐                                      │
│         │          │ sig_updated │                                      │
│         │          │  (signal)   │                                      │
│         │          └──────┬──────┘                                      │
│         │                 │                                             │
│  ┌──────▼─────────────────▼──────┐                                      │
│  │      t_check_convergence      │                                      │
│  └──────┬───────────────┬────────┘                                      │
│         │               │                                               │
│    [continue]      [converged]                                          │
│         │               │                                               │
│         ▼               ▼                                               │
│   (back to ask)   ┌───────────┐                                         │
│                   │  done     │                                         │
│                   │ (terminal)│                                         │
│                   └───────────┘                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

#### Optimizer Adapter Service

The optimizer adapter is a separate service (or embedded component) that:
- Maintains optimizer state (population, history, etc.)
- Responds to `ask` requests with new candidates
- Processes `tell` updates with evaluation results
- Tracks convergence and best-so-far

```python
# What the adapter handles (users never see this)
class OptimizerAdapter:
    def __init__(self, config):
        self.ng_optimizer = ng.optimizers.registry[config.algorithm](
            parametrization=self._build_parametrization(config.parameters),
            budget=config.budget,
            num_workers=config.batch_size,
        )
        self.history = []
        self.best_loss = float('inf')
        self.best_params = None

    async def handle_ask(self, request: AskRequest) -> list[dict]:
        """Generate candidate parameters."""
        candidates = []
        for _ in range(request.batch_size):
            candidate = self.ng_optimizer.ask()
            candidates.append({
                "id": uuid4(),
                "params": candidate.value,
                "_ng_candidate": candidate,  # Internal reference
            })
        return candidates

    async def handle_tell(self, request: TellRequest) -> TellResponse:
        """Process evaluation results."""
        for result in request.results:
            candidate = self._get_candidate(result["id"])
            loss = result["loss"]

            self.ng_optimizer.tell(candidate, loss)
            self.history.append({"params": result["params"], "loss": loss})

            if loss < self.best_loss:
                self.best_loss = loss
                self.best_params = result["params"]

        return TellResponse(
            best_loss=self.best_loss,
            best_params=self.best_params,
            iteration=len(self.history),
            converged=self._check_convergence(),
        )
```

#### Alternative: Inline Optimizer (No External Service)

For simpler cases, the optimizer can run inline within the workflow:

```python
from petri_lab import Workflow, execute, optimizer

with Workflow("simple-optimization") as wf:
    # Inline optimizer - state managed within workflow tokens
    opt = optimizer.inline(
        algorithm="CMA",
        parameters={...},
        budget=100,
    )

    with opt.loop() as lp:
        # Single candidate at a time (simpler, less parallel)
        params = lp.ask_one()

        result = execute("simulate", params=params)
        loss = execute("loss", input=result.output)

        lp.tell_one(params, loss.value)

    wf.output("best", opt.best_params)
```

#### Multi-Fidelity Optimization

Support for multi-fidelity (coarse-to-fine) optimization:

```python
from petri_lab import Workflow, execute, optimizer

with Workflow("multi-fidelity-optimization") as wf:
    opt = optimizer(
        algorithm="BOHB",              # Bayesian Optimization + HyperBand
        parameters={...},
        budget=1000,

        # Fidelity configuration
        fidelity={
            "name": "mesh_size",
            "range": [32, 256],
            "scale": "log",
        },
    )

    with opt.loop() as lp:
        # Ask returns params + fidelity level
        candidates = lp.ask()

        with parallel(candidates) as p:
            # Fidelity affects simulation cost
            sim = execute("mumax3",
                params=p.item.params,
                mesh_size=p.item.fidelity,  # Coarse → Fine
            )
            loss = execute("loss", input=sim.output)
            p.collect({
                "params": p.item.params,
                "fidelity": p.item.fidelity,
                "loss": loss.value,
            })

        lp.tell(p.results)

    wf.output("best", opt.best_params)
```

---

## Complete Example: Optimization Workflow

```python
from petri_lab import (
    Workflow,
    execute,
    parallel,
    loop,
    branch,
    checkpoint,
)

with Workflow("micromagnetic-optimization") as wf:
    # Inputs
    script = wf.input("script", type="file", description="MuMax3 script")
    reference = wf.input("reference", type="file", description="Experimental data")
    budget = wf.input("budget", type="int", default=100)

    # Initialize optimizer
    optimizer = execute("nevergrad-init",
        algorithm="CMA",
        parameters={
            "Aex": {"range": [1e-12, 50e-12], "scale": "log"},
            "alpha": {"range": [0.001, 0.5]},
        },
        budget=budget,
    )

    # Optimization loop
    with loop(max_iterations=budget) as lp:
        # Get next batch of candidates from optimizer
        candidates = execute("nevergrad-ask",
            optimizer=lp.state("optimizer", initial=optimizer.state),
            batch_size=10,
        )

        # Parallel evaluation of candidates
        with parallel(candidates.params) as p:
            # Run simulation
            sim = execute("mumax3", script=script, params=p.item)

            # Compute loss
            loss = execute("compute-loss",
                simulated=sim.output,
                reference=reference,
            )

            p.collect({"params": p.item, "loss": loss.value})

        # Update optimizer with results
        updated = execute("nevergrad-tell",
            optimizer=lp.state("optimizer"),
            results=p.results,
        )

        lp.update("optimizer", updated.state)
        lp.update("best_loss", updated.best_loss)

        # Checkpoint periodically
        with branch(lp.iteration % 10 == 0):
            @branch.case(True)
            def save_checkpoint():
                checkpoint(f"iter-{lp.iteration}", state={
                    "optimizer": updated.state,
                    "best_loss": updated.best_loss,
                })

        # Early stopping
        lp.stop_when(updated.best_loss < 0.001)

    # Final outputs
    wf.output("best_params", lp.final_state["optimizer"].best_params)
    wf.output("best_loss", lp.final_state["best_loss"])
    wf.output("history", lp.final_state["optimizer"].history)
```

---

## Building Block → Petri Net Mapping

| Building Block | Petri Net Implementation |
|----------------|--------------------------|
| `execute()` | Claim pattern + export places + claimed states + retry loop |
| `transfer()` | Claim coordination with storage adapters + checksum verification subnet |
| `branch()` | Guarded transitions with mutually exclusive conditions |
| `loop()` | Cyclic subnet with termination condition guard |
| `parallel()` | Token replication + parallel subnets + synchronization barrier |
| `http()` | Export to HTTP adapter + timeout + retry subnet |
| `checkpoint()` | State snapshot to persistent storage + recovery subnet |
| `timeout()` | Timer adapter + race between completion and timeout |
| `optimizer()` | Ask/tell adapter + shared state + convergence guards + parallel eval subnet |

---

## Implementation Strategy

### Phase 1: PyO3 Bindings for Raw SDK

Expose the existing Rust SDK to Python via PyO3:

```rust
// sdk-python/src/lib.rs
use pyo3::prelude::*;

#[pymodule]
fn petri_lab(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Context>()?;
    m.add_class::<PlaceHandle>()?;
    m.add_class::<TransitionBuilder>()?;
    Ok(())
}
```

### Phase 2: Building Block Components in Rust

Implement building blocks as Rust `Component` implementations:

```rust
// sdk/src/components/execute.rs
pub struct ExecuteNode { ... }

impl Component for ExecuteNode {
    type Input = ExecuteInput;
    type Output = ExecuteOutput;

    fn instantiate(self, ctx: &mut Context, input: Self::Input) -> Self::Output {
        // Build the fault-tolerant subnet
    }
}
```

### Phase 3: Python High-Level API

Wrap building blocks in ergonomic Python functions:

```python
# python/petri_lab/blocks.py

def execute(node: str, *, timeout: str = "1h", retries: int = 3, **inputs) -> ExecuteResult:
    """Execute a node template on the configured scheduler."""
    ctx = _get_current_context()
    component = ExecuteComponent(node, timeout, retries)
    return ctx._use_component(component, inputs)
```

### Phase 4: Platform Integration

Connect to the platform for:
- Node template resolution
- Cluster configuration
- Storage backend routing
- Authentication/authorization

```python
from petri_lab import Platform

platform = Platform.connect("https://lab.example.com", token="...")

with Workflow("my-workflow", platform=platform) as wf:
    # Node templates are resolved from platform
    result = execute("mumax3", script=script)
```

---

## User vs Admin Separation

### What Admins Configure (Platform)

```yaml
# Node templates
apiVersion: petri-lab/v1
kind: NodeTemplate
metadata:
  name: mumax3
spec:
  image: mumax/mumax3:3.10
  resources:
    gpu: 1
    memory: 8GB
  inputs:
    script: { type: file }
    params: { type: json }
  outputs:
    ovf: { type: files, pattern: "*.ovf" }
```

```yaml
# Cluster configuration
apiVersion: petri-lab/v1
kind: ClusterConfig
metadata:
  name: hpc-slurm
spec:
  type: slurm
  host: hpc.university.edu
  partitions:
    gpu: { gpuType: nvidia-a100 }
```

### What Users See

```python
# Users just import and use - no infrastructure config
result = execute("mumax3", script=my_script, params={"Aex": 1e-11})
```

---

## What Users See vs What's Hidden

| User Writes | What's Hidden |
|-------------|---------------|
| `execute("mumax3", ...)` | Claim coordination, submission, polling, retries, timeout, failure routing |
| `transfer(data, to="cluster")` | Claim coordination, checksums, chunking, resume, storage adapter coordination |
| `with branch(cond):` | Guarded transitions, mutex places, token routing |
| `with loop():` | Cyclic subnet, iteration counter, termination guards, state management |
| `with parallel(items):` | Token replication, synchronization barrier, aggregation |
| `http.post(url, ...)` | Retries, backoff, circuit breaker, timeout, error classification |
| `optimizer(...).loop()` | Ask/tell coordination, state persistence, convergence detection, candidate tracking |

---

## Key Insight

Users think in terms of:
- "Run this computation"
- "Move this data"
- "If X then Y else Z"
- "Repeat until done"
- "Do these in parallel"

They don't think about:
- Places, transitions, tokens, arcs
- Guards, export places, claimed states
- Event sourcing, adapters, claim coordination
- Fault tolerance subnets

The building blocks provide the **abstraction boundary**. Platform developers implement building blocks using the raw Petri net SDK. Users compose building blocks without knowing what's underneath.
