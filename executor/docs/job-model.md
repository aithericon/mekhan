# Job Model

An `ExecutionJob` is the unit of work submitted to the executor.

## ExecutionJob

| Field | Type | Description |
|---|---|---|
| `execution_id` | `string` | Caller-assigned unique ID. Used in status subjects and dedup. |
| `spec` | `ExecutionSpec` | Describes what to execute (see below). |
| `metadata` | `map<string, string>` | Opaque key-value pairs echoed in all status updates. |
| `timeout` | `string` (optional) | Human-readable duration (e.g. `"5m"`, `"1h30m"`). Overrides executor default. |
| `priority` | `string` | `"low"`, `"medium"` (default), or `"high"`. Maps to apalis priority queues. |
| `stream_events` | `[string]` (optional) | Event categories to stream in real-time to NATS. When absent, only end-of-execution summaries are published. Categories: `artifact`, `progress`, `phase`, `log`, `output`, `metric`. |
| `wrapped_secrets` | `string` (optional) | Vault wrapping token. When present, `InjectSecretsHook` unwraps it to resolve `{{secret:KEY}}` patterns in `spec.config` and env. Never stored in metadata (which is echoed publicly). |

## ExecutionSpec

The `spec` field describes what to execute. It uses an open `"type"` discriminant — the string selects which backend handles the job, and `config` carries backend-specific parameters as opaque JSON.

| Field | Type | Description |
|---|---|---|
| `type` | `string` | Backend identifier: `"process"`, `"docker"`, `"python"`, `"http"`, `"llm"`, or `"file_ops"`. |
| `inputs` | `[InputDeclaration]` | Input files to stage before execution (default: `[]`). |
| `outputs` | `[OutputDeclaration]` | Expected output files after execution (default: `[]`). |
| `config` | `object` | Backend-specific configuration (see sections below). |

Available backends and their feature flags:

| Backend | `type` value | Feature flag | Description |
|---|---|---|---|
| Process | `process` | *(always enabled)* | Execute arbitrary commands via fork+exec. |
| Docker | `docker` | `docker` | Run in isolated Docker containers. |
| Python | `python` | `python` | Execute Python with optional virtualenv and SDK. |
| HTTP | `http` | `http` | Fire a single HTTP request. |
| LLM | `llm` | `llm` | LLM completions via direct HTTP (OpenAI, Anthropic, Ollama). |
| File Ops | `file_ops` | `file-ops` | Storage file operations (copy, move, delete, etc.). |
| Kreuzberg | `kreuzberg` | `kreuzberg` | Document text extraction (75+ formats via kreuzberg). |

---

## Process (`"type": "process"`)

| Field | Type | Default | Description |
|---|---|---|---|
| `command` | `string` | required | Command to run (e.g. `"python3"`). |
| `args` | `[string]` | `[]` | Command-line arguments. |
| `env` | `map<string, string>` | `{}` | Extra environment variables. |
| `working_dir` | `string` | `null` | Working directory. `null` inherits from executor. |
| `inherit_env` | `bool` | `true` | Inherit executor process environment. |

```json
{
  "type": "process",
  "command": "python3",
  "args": ["train.py", "--epochs", "10"],
  "env": { "CUDA_VISIBLE_DEVICES": "0" },
  "working_dir": "/workspace",
  "inherit_env": true
}
```

---

## Docker (`"type": "docker"`)

| Field | Type | Default | Description |
|---|---|---|---|
| `image` | `string` | required | Docker image (e.g. `"python:3.12-slim"`). |
| `command` | `[string]` | `[]` | Container CMD. |
| `entrypoint` | `[string]` | `null` | Override image entrypoint. |
| `env` | `map<string, string>` | `{}` | Environment variables inside the container. |
| `pull_policy` | `string` | `"if_not_present"` | `"always"`, `"if_not_present"`, or `"never"`. |
| `resource_limits` | `object` | `null` | CPU/memory limits (see below). |
| `network_mode` | `string` | `null` | Docker network mode (`"host"`, `"bridge"`, `"none"`). |
| `extra_volumes` | `[string]` | `[]` | Additional mounts in `"host:container"` format. |
| `remove_container` | `bool` | `true` | Remove container after execution (equivalent to `--rm`). |

### ResourceLimits

| Field | Type | Description |
|---|---|---|
| `memory_bytes` | `int64` | Memory limit in bytes. |
| `cpu_shares` | `int64` | CPU shares (relative weight). |
| `cpu_quota` | `int64` | CPU quota in microseconds per 100ms period (e.g. `50000` = 0.5 CPU). |

The run directory is bind-mounted at `/aithericon` inside the container.

```json
{
  "type": "docker",
  "image": "python:3.12-slim",
  "command": ["python3", "/aithericon/inputs/train.py"],
  "env": { "CUDA_VISIBLE_DEVICES": "0" },
  "pull_policy": "if_not_present",
  "resource_limits": {
    "memory_bytes": 4294967296,
    "cpu_quota": 200000
  },
  "remove_container": true
}
```

---

## Python (`"type": "python"`)

| Field | Type | Default | Description |
|---|---|---|---|
| `script` | `string` | required | Script filename in the inputs directory. |
| `python` | `string` | `"python3"` | Python command/binary. |
| `requirements` | `[string]` | `[]` | Pip packages to install before execution. |
| `virtualenv` | `bool` | `false` | Create an isolated virtualenv. |
| `env` | `map<string, string>` | `{}` | Additional environment variables. |
| `working_dir` | `string` | `null` | Working directory (defaults to run_dir root). |
| `inherit_env` | `bool` | `true` | Inherit executor process environment. |
| `sdk` | `bool` | `true` | Auto-install the aithericon SDK in the virtualenv. Only effective when `virtualenv: true`. |

The Python backend generates a `__runner__.py` wrapper that:
- Loads staged inputs from the inputs directory
- Provides `set_output(name, value)` for file-based output
- Upgrades to IPC-backed SDK functions when `aithericon` is importable (progress, artifacts, metrics, logging)

For inline code, declare the script as a `raw` input named `__script__.py`:

```json
{
  "type": "python",
  "script": "__script__.py",
  "virtualenv": true,
  "requirements": ["numpy", "pandas"],
  "inputs": [
    {
      "name": "__script__.py",
      "source": { "type": "raw", "content": "import pandas as pd\nprint('hello')" }
    }
  ]
}
```

---

## HTTP (`"type": "http"`)

| Field | Type | Default | Description |
|---|---|---|---|
| `method` | `string` | `"GET"` | HTTP method: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`, `HEAD`, `OPTIONS`. |
| `url` | `string` | required | Target URL. Supports `{{variable}}` template substitution. |
| `headers` | `map<string, string>` | `{}` | Request headers. Template substitution in values. |
| `query` | `map<string, string>` | `{}` | URL query parameters. Template substitution in values. |
| `body` | `json` | `null` | Inline request body. String values → `text/plain`; objects/arrays → `application/json`. Mutually exclusive with `body_from_input`. |
| `body_from_input` | `string` | `null` | Name of a staged input file whose contents become the request body. |
| `auth` | `AuthConfig` | `null` | Authentication (see below). |
| `timeout_secs` | `uint64` | `null` | Request-level timeout. Falls back to `RunContext.timeout`. |
| `follow_redirects` | `bool` | `true` | Follow HTTP redirects (up to 10). |
| `expected_status_codes` | `[uint16]` | `[]` | Status codes treated as success. Empty = any 2xx. |
| `response_mode` | `string` | `"auto"` | How to interpret response: `auto`, `json`, `text`, `discard`. |
| `max_response_bytes` | `uint64` | `1048576` | Maximum response body size (1 MB default). |
| `danger_accept_invalid_certs` | `bool` | `false` | Accept invalid TLS certificates. |

### Template Resolution

`{{variable}}` placeholders in `url`, `headers`, and `query` values are resolved during `prepare()`. Lookup order:
1. Environment variables (`RunContext.env`)
2. Staged input file contents (`RunContext.staged_inputs`)
3. Job metadata (`RunContext.metadata`)

Unresolved placeholders cause a config error.

### AuthConfig

Tagged on `"type"`:

**Bearer** (`"type": "bearer"`):
| Field | Description |
|---|---|
| `token` | Inline bearer token. |
| `token_env` | Env var name to load the token from. |

**Basic** (`"type": "basic"`):
| Field | Description |
|---|---|
| `username` | Username. |
| `password` | Inline password. |
| `password_env` | Env var name to load the password from. |

**Header** (`"type": "header"`):
| Field | Description |
|---|---|
| `name` | Header name (e.g. `"X-API-Key"`). |
| `value` | Inline value. |
| `value_env` | Env var name to load the value from. |

### ResponseMode

| Mode | Behavior |
|---|---|
| `auto` | Parse as JSON if `Content-Type` contains `application/json` or `+json`; otherwise text. |
| `json` | Force JSON parsing; error if invalid. |
| `text` | Always treat as text. |
| `discard` | Discard body; only capture status code and headers. |

### Outputs

| Key | Type | Description |
|---|---|---|
| `status_code` | `number` | HTTP status code. |
| `headers` | `map<string, string>` | Response headers. |
| `body` | `json` | Parsed body (structured JSON, string, or null for discard). |
| `content_type` | `string` | Response Content-Type. |
| `response_time_ms` | `number` | Response time in milliseconds. |

### Metrics

`http/status_code`, `http/response_time_ms`, `http/response_bytes`.

```json
{
  "type": "http",
  "method": "POST",
  "url": "https://{{host}}/api/v1/ingest",
  "headers": { "X-Request-Id": "{{execution_id}}" },
  "body": { "data": [1, 2, 3] },
  "auth": { "type": "bearer", "token_env": "API_TOKEN" },
  "expected_status_codes": [200, 201],
  "response_mode": "json"
}
```

---

## LLM (`"type": "llm"`)

| Field | Type | Default | Description |
|---|---|---|---|
| `provider` | `string` | required | LLM provider: `"open_ai"`, `"anthropic"`, or `"ollama"`. |
| `model` | `string` | required | Model identifier (e.g. `"gpt-4o"`, `"claude-sonnet-4-20250514"`). |
| `api_key` | `string` | `null` | API key. Falls back to provider-specific env var (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`). |
| `base_url` | `string` | `null` | Base URL override (for proxies, Azure OpenAI, local Ollama, etc.). |
| `prompt` | `string` | required | User prompt to send to the LLM. Supports `{{input:NAME}}` template substitution. |
| `system_prompt` | `string` | `null` | System prompt (preamble). Supports `{{input:NAME}}` template substitution. |
| `history` | `[ChatMessage]` | `[]` | Prior conversation turns. |
| `temperature` | `float64` | `null` | Sampling temperature. |
| `max_tokens` | `uint64` | `null` | Maximum tokens to generate. |
| `response_format` | `ResponseFormat` | `null` | Response format constraint (see below). `null` or `{"type": "text"}` for free-form text. |
| `images` | `[ImageInput]` | `[]` | Images to include with the user prompt (for vision/OCR models). Each references a staged input file. |

### ImageInput

| Field | Type | Default | Description |
|---|---|---|---|
| `path` | `string` | required | Path to the image file. Use `{{input:NAME}}` to reference a staged input. |
| `media_type` | `string` | `null` | MIME type (e.g. `"image/png"`). Auto-detected from extension if absent. |

### ChatMessage

| Field | Type | Description |
|---|---|---|
| `role` | `string` | `"user"`, `"assistant"`, or `"system"`. |
| `content` | `string` | Message text. |

### ResponseFormat

Tagged on `"type"`:

**text** — Free-form text response (default):
```json
{ "type": "text" }
```

**json_schema** — Structured JSON output constrained by a schema:
| Field | Type | Description |
|---|---|---|
| `schema` | `json` | JSON Schema the response must conform to. |

```json
{ "type": "json_schema", "schema": { "type": "object", "properties": { ... } } }
```

Each provider uses its native structured output mechanism:
- **OpenAI**: `response_format` with `json_schema` type
- **Anthropic**: `tool_use` with a single "extract" tool and `tool_choice: { type: "any" }`
- **Ollama**: `format` parameter with the schema

### Outputs

| Key | Type | Description |
|---|---|---|
| `response` | `json` | LLM response text (text mode) or structured JSON (json_schema mode). |
| `usage` | `object` | Token usage: `input_tokens`, `output_tokens`, `total_tokens`. Always populated. |
| `finish_reason` | `string` | Why the LLM stopped: `stop`, `length`, `content_filter`, or other. |
| `model` | `string` | Model identifier returned by the provider. |

### Metrics

`llm/input_tokens`, `llm/output_tokens`, `llm/total_tokens`. Always populated.

```json
{
  "type": "llm",
  "provider": "anthropic",
  "model": "claude-sonnet-4-20250514",
  "prompt": "Summarize the following data.",
  "system_prompt": "You are a data analyst.",
  "temperature": 0.3,
  "max_tokens": 2048
}
```

**Structured output example:**

```json
{
  "type": "llm",
  "provider": "open_ai",
  "model": "gpt-4o",
  "prompt": "Extract the entities from this text: ...",
  "response_format": {
    "type": "json_schema",
    "schema": {
      "type": "object",
      "properties": {
        "entities": {
          "type": "array",
          "items": { "type": "string" }
        }
      },
      "required": ["entities"]
    }
  }
}
```

**Vision/OCR example:**

```json
{
  "type": "llm",
  "provider": "ollama",
  "model": "glm-ocr:q8_0",
  "prompt": "Extract all text from this document image.",
  "images": [
    { "path": "{{input:document.png}}" }
  ],
  "response_format": {
    "type": "json_schema",
    "schema": {
      "type": "object",
      "properties": {
        "text": { "type": "string" },
        "tables": { "type": "array", "items": { "type": "string" } }
      },
      "required": ["text"]
    }
  }
}
```

---

## File Ops (`"type": "file_ops"`)

The file-ops backend dispatches to one of 7 operations based on the `"operation"` key in the config. Every operation carries its own inline `StorageConfig` — there is no default storage backend.

### StorageConfig

| Field | Type | Default | Description |
|---|---|---|---|
| `backend` | `string` | required | `"local"`, `"s3"`, `"gcs"`, or `"azblob"`. |
| `endpoint` | `string` | required | Endpoint URL. For local: root directory path. |
| `bucket` | `string` | `""` | Bucket/container name (ignored for local). |
| `region` | `string` | `null` | Region (for S3/GCS). |
| `prefix` | `string` | `""` | Path prefix within the bucket. |
| `credentials` | `object` | `{}` | Backend-specific credentials (`access_key`/`secret_key` for S3, etc.). |

### Compression

Copy and move operations support streaming compression/decompression:
- `"gzip"` — Gzip (RFC 1952)
- `"zstd"` — Zstandard (RFC 8878)

Set both `decompress` and `compress` on the same operation to transcode between formats.

### probe

Extract file metadata and checksum.

| Field | Type | Default | Description |
|---|---|---|---|
| `path` | `string` | required | Storage path to probe. |
| `include_statistics` | `bool` | `false` | Include column-level statistics. |
| `storage` | `StorageConfig` | required | Storage backend. |

### copy

Copy a file within or across storage backends. Same-backend copies attempt native `copy()` first and fall back to streaming.

| Field | Type | Default | Description |
|---|---|---|---|
| `source` | `string` | required | Source path. |
| `destination` | `string` | required | Destination path. |
| `source_storage` | `StorageConfig` | required | Source storage backend. |
| `destination_storage` | `StorageConfig` | `null` | Destination storage. Defaults to `source_storage`. |
| `decompress` | `string` | `null` | Decompress source stream (`"gzip"` or `"zstd"`). |
| `compress` | `string` | `null` | Compress destination stream (`"gzip"` or `"zstd"`). |

### move

Move (rename) a file. Attempts atomic `rename()`, then `copy()` + delete, then streaming + delete.

Same fields as `copy`.

### delete

| Field | Type | Default | Description |
|---|---|---|---|
| `path` | `string` | required | Storage path to delete. |
| `ignore_missing` | `bool` | `false` | Don't error if file doesn't exist. |
| `storage` | `StorageConfig` | required | Storage backend. |

### annotate

Write or merge a `.meta.json` sidecar file.

| Field | Type | Default | Description |
|---|---|---|---|
| `path` | `string` | required | Target file path (must exist). |
| `annotations` | `map<string, json>` | required | Key-value annotations. |
| `merge` | `bool` | `false` | Deep-merge with existing sidecar (vs. overwrite). |
| `storage` | `StorageConfig` | required | Storage backend. |

### list

List files under a storage prefix.

| Field | Type | Default | Description |
|---|---|---|---|
| `prefix` | `string` | required | Storage prefix to list. |
| `limit` | `uint64` | `null` | Maximum entries to return. |
| `include_stat` | `bool` | `false` | Include size and last_modified per entry. |
| `storage` | `StorageConfig` | required | Storage backend. |

### stat

Get file metadata (existence, size, last modified, content type, etag). Non-existent files return `{ "exists": false }` (not an error).

| Field | Type | Default | Description |
|---|---|---|---|
| `path` | `string` | required | Storage path. |
| `storage` | `StorageConfig` | required | Storage backend. |

**Examples:**

```json
{
  "type": "file_ops",
  "operation": "copy",
  "source": "raw/data.csv",
  "destination": "archive/data.csv.gz",
  "source_storage": { "backend": "s3", "endpoint": "https://s3.amazonaws.com", "bucket": "data" },
  "compress": "gzip"
}
```

```json
{
  "type": "file_ops",
  "operation": "stat",
  "path": "datasets/train.parquet",
  "storage": { "backend": "s3", "endpoint": "https://s3.amazonaws.com", "bucket": "ml-data" }
}
```

---

## Kreuzberg (`"type": "kreuzberg"`)

Extracts text, metadata, and tables from documents using the kreuzberg library. Supports 75+ file formats including PDF, Office documents, images (with OCR), email, HTML, XML, and archives.

### Config

| Field | Type | Default | Description |
|---|---|---|---|
| `mode` | `string` | `"single"` | `"single"` or `"batch"`. |
| `file` | `string` | `null` | Input name for single mode. Defaults to `"file"` or the sole staged input. |
| `files` | `[string]` | `[]` | Input names for batch mode. Empty = all staged inputs. |
| `mime_type` | `string` | `null` | MIME type override. Auto-detected from file extension if absent. |
| `force_ocr` | `bool` | `false` | Force OCR even on text-based PDFs. |
| `ocr` | `object` | `null` | OCR settings: `backend` (`"tesseract"` / `"paddle-ocr"`), `language` (ISO 639-3, default `"eng"`), `enable_table_detection`. |
| `pdf` | `object` | `null` | PDF settings: `passwords` (array of strings to try). |

### Input Resolution (Single Mode)

1. If `file` is set in config, use that input name.
2. If there is exactly one staged input, use it regardless of name.
3. If there is an input named `"file"`, use it.
4. Otherwise, error.

### Outputs (Single Mode)

| Key | Type | Description |
|---|---|---|
| `content` | `string` | Extracted text. |
| `mime_type` | `string` | Detected MIME type. |
| `tables` | `[object]` | Tables with `markdown`, `page_number`, `rows`. |
| `table_count` | `number` | Number of tables. |
| `word_count` | `number` | Word count. |
| `char_count` | `number` | Character count. |
| `detected_languages` | `[string]` | Detected languages. |
| `metadata` | `object` | Native kreuzberg metadata. |

### Outputs (Batch Mode)

| Key | Type | Description |
|---|---|---|
| `results` | `[object]` | Per-file results: `file`, `content`, `tables`, `word_count`, etc. |
| `total_files` | `number` | Total files processed. |
| `successful` | `number` | Successfully extracted. |
| `failed` | `number` | Failed extractions. |
| `errors` | `[object]` | `{file, error}` for failures. |

### Metrics

Single: `kreuzberg/extraction_time_ms`, `kreuzberg/content_length`, `kreuzberg/word_count`, `kreuzberg/table_count`.

Batch: `kreuzberg/total_extraction_time_ms`, `kreuzberg/total_files`, `kreuzberg/successful_files`, `kreuzberg/failed_files`, `kreuzberg/total_content_length`, `kreuzberg/total_table_count`.

```json
{
  "type": "kreuzberg",
  "inputs": [
    { "name": "file", "source": { "type": "storage_path", "path": "uploads/contract.pdf" } }
  ],
  "outputs": [
    { "name": "content", "required": true },
    { "name": "tables", "required": false }
  ],
  "config": {}
}
```

**OCR + password example:**

```json
{
  "type": "kreuzberg",
  "inputs": [
    { "name": "document", "source": { "type": "storage_path", "path": "scanned/invoice.pdf" } }
  ],
  "config": {
    "file": "document",
    "force_ocr": true,
    "ocr": { "backend": "tesseract", "language": "deu", "enable_table_detection": true },
    "pdf": { "passwords": ["secret123"] }
  }
}
```

**Batch example:**

```json
{
  "type": "kreuzberg",
  "inputs": [
    { "name": "report_q1", "source": { "type": "storage_path", "path": "reports/q1.pdf" } },
    { "name": "report_q2", "source": { "type": "storage_path", "path": "reports/q2.pdf" } }
  ],
  "outputs": [
    { "name": "results", "required": true }
  ],
  "config": {
    "mode": "batch"
  }
}
```

---

## InputDeclaration

| Field | Type | Default | Description |
|---|---|---|---|
| `name` | `string` | required | Used as filename in the `inputs/` directory. |
| `source` | `InputSource` | required | Where to get the data. |
| `required` | `bool` | `true` | Fail staging if input cannot be resolved. |

### InputSource variants

```json
{ "type": "inline", "value": {"learning_rate": 0.001} }
{ "type": "raw", "content": "print('hello')" }
{ "type": "storage_path", "path": "artifacts/prev-run/model.pt" }
{ "type": "url", "url": "https://example.com/data.csv" }
```

- **inline** — JSON value written as a file in `inputs/`.
- **raw** — Raw text content written verbatim (no JSON serialization).
- **storage_path** — Downloaded from the `ArtifactStore`.
- **url** — Downloaded from a URL (not yet implemented).

## OutputDeclaration

| Field | Type | Default | Description |
|---|---|---|---|
| `name` | `string` | required | Logical output name. |
| `path` | `string` | `null` | Relative path within `outputs/`. `null` means value is set via IPC. |
| `required` | `bool` | `true` | Fail if output is missing after execution. |

---

## Full Job Examples

### Process job

```json
{
  "execution_id": "train-alpha-0",
  "spec": {
    "type": "process",
    "command": "python3",
    "args": ["train.py", "--epochs", "10"],
    "env": { "CUDA_VISIBLE_DEVICES": "0" },
    "inputs": [
      {
        "name": "config.json",
        "source": { "type": "inline", "value": {"learning_rate": 0.001} }
      }
    ],
    "outputs": [
      { "name": "model.pt", "path": "model.pt" },
      { "name": "accuracy" }
    ]
  },
  "metadata": { "petri_net_id": "net-1", "transition_id": "t-train" },
  "timeout": "2h",
  "priority": "high"
}
```

### HTTP job

```json
{
  "execution_id": "webhook-notify-1",
  "spec": {
    "type": "http",
    "method": "POST",
    "url": "https://api.example.com/webhook",
    "body": { "event": "training_complete", "model_id": "alpha-0" },
    "auth": { "type": "bearer", "token_env": "WEBHOOK_TOKEN" },
    "expected_status_codes": [200, 202]
  },
  "metadata": { "petri_net_id": "net-1" }
}
```

### LLM job

```json
{
  "execution_id": "summarize-results-1",
  "spec": {
    "type": "llm",
    "provider": "anthropic",
    "model": "claude-sonnet-4-20250514",
    "prompt": "Summarize these experiment results.",
    "system_prompt": "You are a scientific writer.",
    "max_tokens": 1024
  },
  "metadata": { "experiment_id": "exp-42" }
}
```

### File ops job

```json
{
  "execution_id": "archive-dataset-1",
  "spec": {
    "type": "file_ops",
    "operation": "copy",
    "source": "datasets/train.csv",
    "destination": "archive/train.csv.zst",
    "source_storage": {
      "backend": "s3",
      "endpoint": "https://s3.amazonaws.com",
      "bucket": "ml-data",
      "region": "us-east-1"
    },
    "compress": "zstd"
  },
  "metadata": { "triggered_by": "retention-policy" }
}
```

### Kreuzberg job

```json
{
  "execution_id": "extract-contract-1",
  "spec": {
    "type": "kreuzberg",
    "inputs": [
      { "name": "file", "source": { "type": "storage_path", "path": "contracts/agreement.pdf" } }
    ],
    "outputs": [
      { "name": "content", "required": true },
      { "name": "tables", "required": false }
    ],
    "config": {
      "force_ocr": true,
      "ocr": { "language": "eng", "enable_table_detection": true }
    }
  },
  "metadata": { "document_type": "contract" }
}
```

## Serde Conventions

- Tagged enums use `"type"` as the discriminant field (e.g. `ExecutionSpec`, `AuthConfig`, `InputSource`).
- The file-ops `FileOpsConfig` uses `"operation"` as its discriminant.
- All field names are `snake_case`.
- Durations are human-readable strings (`"5m"`, `"1h30m"`, `"30s"`).
- Optional fields are omitted from JSON when `null`/empty.

## Backward Compatibility

`inputs`, `outputs`, `stream_events`, and `wrapped_secrets` default to empty/null. Old JSON without these fields deserializes correctly.
