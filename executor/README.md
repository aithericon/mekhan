# Aithericon Executor

Distributed task executor service. Receives execution jobs via NATS JetStream (through an apalis job queue), dispatches them to pluggable backends, and publishes structured status updates back via NATS.

## Prerequisites

- Rust toolchain (stable)
- Docker (for NATS JetStream)
- [`just`](https://github.com/casey/just) task runner
- Native build deps (HDF5, NetCDF, protobuf, …) — see [`../docs/setup.md`](../docs/setup.md)

This crate lives in the [Mekhan monorepo](https://github.com/aithericon/mekhan).
Its path-dependencies (`aithericon-secrets`, `fmeta`, and the `apalis` NATS fork)
are vendored in-tree under `../shared/`, so a single clone of the monorepo is all
you need — there are no sibling repos to clone.

## Quick Start

```bash
git clone https://github.com/aithericon/mekhan
cd mekhan/executor

just nats-up        # Start NATS JetStream in Docker
just run-debug      # Run with RUST_LOG=debug
```

Or, from the monorepo root, `just dev` brings up the whole stack (NATS, engine,
mekhan, app) with the executor already wired in. See [CLAUDE.md](CLAUDE.md) for
the full list of build and development commands.

## External Dependencies

This workspace uses Cargo path dependencies (declared in the workspace `Cargo.toml` under `[workspace.dependencies]`) to crates vendored elsewhere in the monorepo under `../shared/`:

- `aithericon-secrets` (`../shared/secrets`) and `aithericon-file-metadata` (`../shared/file-metadata`, package `fmeta`) — Apache-2.0 Aithericon crates.
- `apalis`, `apalis-core`, `apalis-nats` (`../shared/apalis`) — a fork of [apalis](https://github.com/geofmureithi/apalis) with a NATS JetStream backend. Upstream apalis does not yet include a NATS backend.

For use outside the monorepo, replace these path dependencies with `git = "..."` or published-crate dependencies as appropriate.

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
