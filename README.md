# Aithericon Platform

A Colored Petri-net backed workflow engine with a real-time collaborative editor,
designed for SOP-style processes with auditable provenance.

> **⚠️ Early days — work in progress.** This is an open research/experimentation
> drop. APIs, schemas, and the UI are changing fast; expect breaking changes
> between commits. Nothing here is production-ready. We're sharing it now to
> develop in the open, not because it's stable.

## What's here

| Directory | What it is |
|-----------|-----------|
| [`engine/`](./engine/) | Petri-net execution engine, SDK, CLI, simulator (Apache-2.0) |
| [`executor/`](./executor/) | Task/job executor (Apache-2.0) |
| [`service/`](./service/) | Control plane / orchestrator — Axum + Postgres + NATS + Yjs (FSL-1.1-ALv2) |
| [`app/`](./app/) | SvelteKit frontend — Svelte 5, xyflow, Yjs (FSL-1.1-ALv2) |
| [`shared/`](./shared/) | Vendored `apalis` fork, secrets plumbing, file metadata |
| [`docs/`](./docs/) | Architecture & design notes |

## Quick start

```bash
docker compose up -d              # Postgres + NATS
cd service && cargo run           # backend
cd app && pnpm install && pnpm dev   # frontend (separate terminal)
```

Native build deps (HDF5, NetCDF, protobuf, etc.) are listed in
[`docs/setup.md`](./docs/setup.md). Nix users: `nix develop` gives you
everything.

## Licensing

Multi-licensed per crate. **Open-source engine & SDK (Apache-2.0)**,
**source-available control plane** (FSL-1.1-ALv2, converts to Apache-2.0 two
years after each release). See [`LICENSING.md`](./LICENSING.md) for the
per-crate table and the rationale.

## Contributing

See [`CONTRIBUTING.md`](./CONTRIBUTING.md). Contributions go in under
inbound=outbound license with a DCO sign-off (`git commit -s`).

#test

#test


#test