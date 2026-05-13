# Backend Trait

The `ExecutionBackend` trait defines how the executor runs workloads. Each backend handles one or more `ExecutionSpec` variants.

## ExecutionBackend

```rust
#[async_trait]
pub trait ExecutionBackend: Send + Sync + 'static {
    /// Backend-specific preparation. Called AFTER shared staging hooks.
    /// Default: no-op, returns ctx unchanged.
    async fn prepare(&self, job: &ExecutionJob, run_context: RunContext)
        -> Result<RunContext, ExecutorError> {
        Ok(run_context)
    }

    /// Execute within the prepared context.
    async fn execute(&self, run_context: &RunContext, status_cb: StatusCallback,
                     cancel: CancellationToken) -> Result<ExecutionResult, ExecutorError>;

    /// Human-readable backend name (e.g., "process", "docker").
    fn name(&self) -> &'static str;

    /// Whether this backend can handle the given spec variant.
    fn supports(&self, spec: &ExecutionSpec) -> bool;
}
```

## StatusCallback

```rust
pub type StatusCallback = Box<
    dyn Fn(ExecutionStatus, Value) -> Pin<Box<dyn Future<Output = ()> + Send>>
    + Send + Sync
>;
```

Backends call `status_cb(ExecutionStatus::Running, json!({"pid": pid}))` to report mid-execution status. The callback publishes to NATS — backends never touch NATS directly.

## Contract

### prepare()

- Called after all shared staging hooks have run.
- Use this for backend-specific setup (e.g., resolving templates, pulling images, creating virtualenvs).
- Default implementation is a no-op.
- May modify `RunContext.env`, `RunContext.backend_state`, etc.

### execute()

- Must call `status_cb(Running, ...)` once execution starts.
- Must respect the `cancel` token.
- Must return `ExecutionResult` with `outcome`, `duration`, and optional stdout/stderr tails.
- Must not block indefinitely — respect the timeout in `run_context.timeout`.

### supports()

- Return `true` if this backend can handle the given `ExecutionSpec`.
- `BackendRegistry` calls this to find the right backend.

## ExecutionResult

| Field | Type | Description |
|---|---|---|
| `outcome` | `ExecutionOutcome` | What happened (see below). |
| `duration` | `Duration` | Wall-clock execution time. |
| `stdout_tail` | `Option<String>` | Last N bytes of stdout. |
| `stderr_tail` | `Option<String>` | Last N bytes of stderr. |
| `artifact_manifest` | `Option<ArtifactManifest>` | Artifacts collected via IPC. |
| `outputs` | `HashMap<String, Value>` | Output values (IPC-set or backend-specific). |
| `progress` | `Option<Progress>` | Final progress state. |
| `run_dir` | `Option<RunDirectory>` | Run directory used. |
| `metrics` | `Option<MetricSummary>` | Metrics summary (metric names + latest values). |
| `logs` | `Option<LogSummary>` | Log summary (counts by level, recent errors). |

## ExecutionOutcome

| Variant | Maps to Status | Description |
|---|---|---|
| `Success` | `Completed` | Process exited 0 / HTTP 2xx / LLM response OK. |
| `ExitFailure { exit_code }` | `Failed` | Process exited non-zero / HTTP non-success status. |
| `Signal { signal }` | `Failed` | Process killed by signal. |
| `TimedOut` | `TimedOut` | Exceeded timeout. |
| `BackendError { message }` | `Failed` | Backend-level error (e.g., spawn failure, connection refused). |
| `Cancelled` | `Cancelled` | Cancelled via token. |

## BackendRegistry

```rust
let registry = BackendRegistry::new(Duration::from_secs(3600))
    .register(ProcessBackend::new())
    .register(PythonBackend::new())
    .register(HttpBackend::new());
```

`BackendRegistry::find(&spec)` iterates registered backends and returns the first whose `supports()` returns `true`. The registry also holds the `default_timeout` used when a job doesn't specify one.

Registration order in `executor-service`:
1. `ProcessBackend` (always)
2. `PythonBackend` (feature: `python`)
3. `DockerBackend` (feature: `docker`)
4. `LlmBackend` (feature: `llm`)
5. `HttpBackend` (feature: `http`)
6. `FileOpsBackend` (feature: `file-ops`)
7. `KreuzbergBackend` (feature: `kreuzberg`)

## Adding a New Backend

1. Pick a unique `backend` string (e.g., `"my_backend"`).
2. Define a config struct with `Serialize`/`Deserialize` that maps to your `ExecutionSpec.config`.
3. Implement `ExecutionBackend` for your type.
4. Register it with `BackendRegistry::register()` in `executor-service`.

Example skeleton:

```rust
pub struct MyBackend;

#[async_trait]
impl ExecutionBackend for MyBackend {
    fn name(&self) -> &'static str { "my_backend" }

    fn supports(&self, spec: &ExecutionSpec) -> bool {
        spec.backend == "my_backend"
    }

    async fn execute(
        &self,
        run_context: &RunContext,
        status_cb: StatusCallback,
        cancel: CancellationToken,
    ) -> Result<ExecutionResult, ExecutorError> {
        status_cb(ExecutionStatus::Running, json!({})).await;
        // ... your execution logic ...
    }
}
```

---

## ProcessBackend

The built-in backend for local process execution (fork+exec).

- **prepare()**: No-op — returns context unchanged.
- **execute()**: Spawns the command with `tokio::process::Command`.
  - Pipes stdout/stderr through `TailBuffer` (ring buffer, default 64 KB per stream).
  - Injects environment from `RunContext.env` and `ProcessConfig.env`.
  - Reports `Running` with PID.
  - Handles timeout: SIGTERM, 5-second grace, SIGKILL.
  - Handles cancellation via `CancellationToken`.
- **Outputs**: None (use IPC `SetOutput` from child process).
- **Metrics**: None (use IPC `LogMetric` from child process).

## DockerBackend

Runs jobs inside Docker containers via the bollard client.

- **prepare()**: Pulls the image according to `pull_policy` (`always`, `if_not_present`, `never`).
- **execute()**: Creates a container named `aithericon-{execution_id}`.
  - Bind-mounts the run directory to `/aithericon` inside the container.
  - Injects environment from `DockerConfig.env`.
  - Reports `Running` with container ID.
  - Waits for container exit, timeout, or cancellation.
  - Captures logs via `TailBuffer` (default 64 KB per stream).
  - Removes container after execution if `remove_container: true`.
- **Outputs**: None (use IPC from within the container).
- **Metrics**: None.

## PythonBackend

Executes Python code with optional virtualenv isolation and SDK integration.

- **prepare()**:
  - If `virtualenv: true`: creates virtualenv, installs pip requirements, optionally installs the aithericon SDK.
  - Locates the user script in the inputs directory.
  - Generates a `__runner__.py` wrapper that provides `set_output()`, loads inputs, and upgrades to SDK functions when available.
  - Stores `python_bin` and `runner_path` in `backend_state`.
- **execute()**: Builds a `ProcessConfig` wrapping the runner and delegates to the shared `run_process()` engine.
  - Command: `{python_bin} -u {runner_path}`
  - Same timeout/cancellation/output-capture as ProcessBackend.
- **Outputs**: Set via file-based `set_output(name, value)` or IPC SDK.
- **Metrics**: Set via IPC SDK `log_metric()`.

## HttpBackend

Executes a single HTTP request via reqwest.

- **prepare()**:
  - Validates config (URL required, `body`/`body_from_input` mutual exclusivity).
  - Resolves auth tokens from environment variables.
  - Resolves `{{variable}}` templates in URL, headers, and query params.
  - Stores `ResolvedHttpConfig` in `backend_state`.
- **execute()**: Builds a reqwest client and request, then runs a three-way `tokio::select!` (cancellation, timeout, HTTP request).
  - Reports `Running` with method and URL.
  - On success: processes response (status code, headers, body parsing per `response_mode`, size limiting).
  - Non-2xx status (unless in `expected_status_codes`) → `ExitFailure { exit_code: status_code }`.
  - Connection errors → `BackendError`.
- **Outputs**: `status_code`, `headers`, `body`, `content_type`, `response_time_ms`.
- **Metrics**: `http/status_code`, `http/response_time_ms`, `http/response_bytes`.

## LlmBackend

Executes LLM completions via direct HTTP calls. Supports OpenAI, Anthropic, and Ollama providers through a hexagonal `CompletionPort` trait with provider-specific adapters.

- **prepare()**:
  - Validates config (json_schema response_format requires a non-null schema).
  - Resolves `{{input:NAME}}` template patterns in prompt, system_prompt, and image paths.
  - Stores validated `LlmConfig` in `backend_state`.
- **execute()**: Creates the appropriate adapter, merges config api_key/base_url into env, then runs a three-way `tokio::select!` (cancellation, timeout, completion call).
  - If `images` are configured, reads staged files, base64-encodes them, and attaches them to the user message. MIME types are auto-detected from file extensions.
  - Reports `Running` with provider name and model.
  - Text mode (no response_format): `outputs["response"]` is the text string.
  - JsonSchema mode: `outputs["response"]` is the structured JSON value.
  - Provider errors → `BackendError`.
- **Outputs**: `response`, `usage` (`input_tokens`/`output_tokens`/`total_tokens`), `finish_reason`, `model`.
- **Metrics**: `llm/input_tokens`, `llm/output_tokens`, `llm/total_tokens` (always populated).
- **Vision/OCR**: Supports sending images alongside prompts for OCR and multimodal tasks. See [LLM Vision & OCR](llm-vision.md) for full documentation.

## FileOpsBackend

Executes general-purpose file operations via OpenDAL. Stateless dispatcher — each operation config carries its own `StorageConfig`.

- **prepare()**: Deserializes and validates the `FileOpsConfig`. Stores it in `backend_state`.
- **execute()**: Three-way `tokio::select!` (cancellation, timeout, operation dispatch).
  - Reports `Running` with operation name.
  - Dispatches to the appropriate operation handler (probe, copy, move, delete, annotate, list, stat).
  - Operation errors → `BackendError`.
- **Outputs**: Operation-specific (e.g., stat returns `exists`, `size_bytes`, `last_modified`; list returns `entries`; probe returns file metadata).
- **Metrics**: None.

## KreuzbergBackend

Extracts text, metadata, and tables from documents via the [kreuzberg](https://github.com/nicholasgasior/kreuzberg) Rust library. Supports 75+ file formats (PDF, Office, images w/ OCR, email, archives, etc.) in an in-process backend — no subprocess or HTTP server needed.

- **prepare()**:
  - Resolves `{{input:NAME}}` template patterns in config.
  - Deserializes `KreuzbergConfig` and validates target file(s) exist in `staged_inputs`.
  - Single mode: resolves the input name (explicit `file` field, sole input, or default `"file"` name).
  - Batch mode: resolves target list (explicit `files` list, or all staged inputs).
  - Stores `ResolvedKreuzbergConfig` in `backend_state`.
- **execute()**: Three-way `tokio::select!` (cancellation, timeout, extraction).
  - Single mode: calls `kreuzberg::extract_file()`, populates outputs with `content`, `tables`, `metadata`, `word_count`, etc.
  - Batch mode: iterates files sequentially with per-file progress reporting via `status_cb`. Partial failures (some succeed, some fail) are `Success`; total failure is `BackendError`.
  - Reports `Running` with mode and file info.
  - Undeclared spec outputs are mapped to `content` (like LLM maps to `response`).
  - Writes to `expected_outputs` file paths.
  - Extraction errors → `BackendError`.
- **Outputs (single)**: `content`, `mime_type`, `tables`, `table_count`, `word_count`, `char_count`, `detected_languages`, `metadata`.
- **Outputs (batch)**: `results` (array of per-file results), `total_files`, `successful`, `failed`, `errors`.
- **Metrics (single)**: `kreuzberg/extraction_time_ms`, `kreuzberg/content_length`, `kreuzberg/word_count`, `kreuzberg/table_count`.
- **Metrics (batch)**: `kreuzberg/total_extraction_time_ms`, `kreuzberg/total_files`, `kreuzberg/successful_files`, `kreuzberg/failed_files`, `kreuzberg/total_content_length`, `kreuzberg/total_table_count`.
