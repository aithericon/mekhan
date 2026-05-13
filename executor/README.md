# Aithericon Executor

Distributed task executor service. Receives execution jobs via NATS JetStream (through an apalis job queue), dispatches them to pluggable backends, and publishes structured status updates back via NATS.

## Prerequisites

- Rust toolchain (stable)
- Docker (for NATS JetStream)
- [`just`](https://github.com/casey/just) task runner
- Sibling clones of these repositories (path-dependencies, see note below):
  - [`aithericon-secrets`](https://github.com/aithericon/aithericon-secrets) at `../aithericon-secrets`
  - [`fmeta`](https://github.com/aithericon/fmeta) at `../file-metadata` (the repo is named `fmeta`; clone it as `file-metadata` locally to match the path-dependency)
  - A fork of [apalis](https://github.com/geofmureithi/apalis) at `../apalis` that provides a NATS backend (`apalis-nats`). This is currently a local fork; see [external dependencies](#external-dependencies) below.

## Quick Start

```bash
# From a parent directory that will hold all sibling repos:
git clone https://github.com/aithericon/aithericon-executor
git clone https://github.com/aithericon/aithericon-secrets
git clone https://github.com/aithericon/fmeta file-metadata
# plus the apalis fork with NATS backend at ../apalis

cd aithericon-executor
just nats-up        # Start NATS JetStream in Docker
just run-debug      # Run with RUST_LOG=debug
```

See [CLAUDE.md](CLAUDE.md) for the full list of build and development commands.

## External Dependencies

This workspace currently uses Cargo path dependencies to three sibling repositories (declared in the root `Cargo.toml` under `[workspace.dependencies]`):

- `aithericon-secrets` and `aithericon-file-metadata` — the public Aithericon crates linked above.
- `apalis`, `apalis-core`, `apalis-nats` — a local fork of apalis with a NATS JetStream backend. Upstream apalis does not yet include a NATS backend.

For use outside the Aithericon monorepo, replace these path dependencies with `git = "..."` or published-crate dependencies as appropriate.

## Crates

| Crate | Description |
|---|---|
| `executor-domain` | Pure data types (jobs, status, results, artifacts) — no I/O |
| `executor-ipc` | FlatBuffers IPC protocol over Unix sockets |
| `executor-storage` | `ArtifactStore` trait and local filesystem implementation |
| `executor-backend` | `ExecutionBackend` trait and `ProcessBackend` (fork+exec) |
| `executor-llm` | LLM completions backend (OpenAI, Anthropic, Ollama) — feature-gated |
| `executor-file-ops` | Storage file operations backend (copy, move, delete, etc.) — feature-gated |
| `executor-kreuzberg` | Document text extraction backend (75+ formats via kreuzberg) — feature-gated |
| `executor-worker` | Orchestration: staging pipeline, IPC sidecar, status reporting |
| `executor-service` | Binary entry point — wires NATS, apalis, and backends |
| `executor-test-harness` | Integration test utilities with NATS testcontainers |

## Documentation

Contract and architecture documentation lives in [`docs/`](docs/README.md).

## Contributing

Issues and pull requests are welcome. Please open an issue to discuss substantial changes before starting work.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work shall be licensed as Apache-2.0, without any additional terms or conditions.
