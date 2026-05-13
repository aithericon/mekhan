# Run Directory

Each execution gets an isolated directory tree at `{base_dir}/runs/{execution_id}/`.

## Layout

```
{base_dir}/runs/{execution_id}/
├── context.json       # Serialized RunContext (read-only for child)
├── inputs/            # Staged input files (one file per InputDeclaration)
├── outputs/           # Child writes declared outputs here
├── artifacts/         # Child writes artifact files here
├── logs/
│   ├── stdout.log
│   └── stderr.log
└── ipc.sock           # Unix domain socket for IPC
```

All directories are created by the `CreateRunDirectoryHook` before execution starts. The child process can rely on every directory existing.

## context.json

The `WriteContextHook` serializes the full `RunContext` to `context.json`. This file is read-only for the child process — it describes the execution environment.

### RunContext fields

| Field | Type | Description |
|---|---|---|
| `execution_id` | `string` | Execution identifier. |
| `spec` | `ExecutionSpec` | The job spec being executed. |
| `run_dir` | `RunDirectory` | All computed paths (root, inputs_dir, outputs_dir, etc). |
| `timeout` | `string` | Human-readable timeout duration. |
| `env` | `map<string, string>` | Accumulated environment variables. |
| `metadata` | `map<string, string>` | Echoed from the job. |
| `staged_inputs` | `map<string, path>` | Input name → local file path. |
| `expected_outputs` | `map<string, path>` | Output name → expected file path. |
| `backend_state` | `json` | Opaque backend-specific state. |

## Environment Variables

The `InjectEnvironmentHook` sets these variables on the `RunContext`, which are then injected into the child process environment:

| Variable | Value |
|---|---|
| `AITHERICON_EXECUTION_ID` | The execution_id string |
| `AITHERICON_RUN_DIR` | Root of the run directory |
| `AITHERICON_IPC_SOCKET` | Path to `ipc.sock` |
| `AITHERICON_INPUTS_DIR` | Path to `inputs/` |
| `AITHERICON_OUTPUTS_DIR` | Path to `outputs/` |
| `AITHERICON_ARTIFACTS_DIR` | Path to `artifacts/` |

### Precedence

Environment variables are applied in this order (later wins):

1. Inherited from executor process (if `inherit_env: true` in ProcessSpec)
2. `ProcessSpec.env` — variables declared in the job spec
3. `RunContext.env` — variables set by staging hooks (including AITHERICON_*)
