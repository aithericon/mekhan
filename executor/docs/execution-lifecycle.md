# Execution Lifecycle

## Status State Machine

```
              ┌───────────┐
              │  Accepted  │
              └─────┬─────┘
                    │
              ┌─────▼─────┐
              │  Running   │
              └─────┬─────┘
                    │
       ┌────────┬───┴───┬────────┐
       ▼        ▼       ▼        ▼
 ┌──────────┐ ┌──────┐ ┌───────┐ ┌──────────┐
 │Completed │ │Failed│ │TimedOut│ │Cancelled │
 └──────────┘ └──────┘ └───────┘ └──────────┘
    terminal   terminal  terminal   terminal
```

- **Accepted** — Job received, backend found, staging about to begin.
- **Running** — Backend has started execution (e.g., process spawned).
- **Completed** — Process exited 0 (success).
- **Failed** — Process exited non-zero, killed by signal, or backend error.
- **TimedOut** — Execution exceeded its timeout.
- **Cancelled** — Execution was cancelled via `CancellationToken`.

Terminal statuses have `is_terminal() == true`. Each execution transitions through each status at most once.

## Handler Flow

The `handle_execution` function in `executor-worker` orchestrates the full lifecycle:

1. **Report Accepted** — Publish `Accepted` status to NATS.
2. **Find backend** — `BackendRegistry.find(&job.spec)` returns the first backend whose `supports()` matches. If none found, report `Failed` and return.
3. **Build RunContext** — Create `RunDirectory` paths, set timeout (job-specified or executor default), copy metadata.
4. **Run staging pipeline** — Execute all `StagingHook`s in order, then `backend.prepare()`. If staging fails, report `Failed` and return.
5. **Build StatusCallback** — Wrap the `StatusReporter` in a closure for the backend.
6. **Execute** — Call `backend.execute(&run_context, status_cb, cancel)`.
7. **Map outcome** — Convert `ExecutionOutcome` to terminal `ExecutionStatus`.
8. **Report terminal status** — Publish `Completed`, `Failed`, `TimedOut`, or `Cancelled` with the execution result details.
9. **Return Ok(())** — Execution failures are application outcomes, not infrastructure errors. The handler always returns `Ok(())` so apalis does not retry.

## Staging Pipeline

The `StagingPipeline` runs an ordered list of `StagingHook` implementations, followed by `backend.prepare()`. Each hook receives the `RunContext` and returns a (possibly modified) `RunContext`.

### Built-in hooks (in order)

| # | Hook | What it does |
|---|---|---|
| 1 | `CreateRunDirectoryHook` | Creates the run directory tree (`mkdir -p` for all subdirs). |
| 2 | `InjectEnvironmentHook` | Sets `AITHERICON_*` env vars on `RunContext.env`. |
| 3 | `StageInputsHook` | Writes inline inputs to `inputs/`, records expected outputs. |
| 4 | `WriteContextHook` | Serializes `RunContext` to `context.json` in the run directory. |
| — | `backend.prepare()` | Backend-specific preparation (e.g., ProcessBackend sets working_dir). |

### StagingHook trait

```rust
#[async_trait]
pub trait StagingHook: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    async fn stage(&self, job: &ExecutionJob, ctx: RunContext)
        -> Result<RunContext, ExecutorError>;
}
```

Custom hooks can be added to the pipeline via `StagingPipeline::add_hook()`.

## Timeout and Cancellation

When a process exceeds its timeout (or cancellation is requested):

1. **SIGTERM** sent to the process.
2. **5-second grace period** for the process to clean up.
3. **SIGKILL** if the process is still running after the grace period.

The resulting `ExecutionOutcome` is `TimedOut` or `Cancelled` respectively.
