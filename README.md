# Aithericon Platform

**A unified control plane for real-world work.** Aithericon integrates the
infrastructure an organization already runs — people, HPC clusters, lab
instruments, workstations, edge devices, and AI model servers — into one system,
so a single workflow can **orchestrate** all of it, **observe** exactly what
happened, and **govern** it with end-to-end provenance.

Built for research and industry teams whose processes span people *and*
machines, and need to be durable, reproducible, and auditable.

> **⚠️ Early alpha — work in progress.** APIs, schemas, and the UI are changing
> fast; expect breaking changes between commits. Nothing here is
> production-ready, and the default dev stack is insecure by design (see the
> **Security & maturity** section below). We're sharing it now to develop in the
> open, not because it's stable.

Aithericon turns a process into a **durable, executable graph**. Workflows run
on an event-sourced engine that persists every step as it happens — so a
long-running process can stretch across hours or days and span flaky networks,
preemptible HPC nodes, and intermittently-connected lab hardware **without
losing state**. If a worker dies, a cluster reclaims a node, or the control
plane restarts, the run resumes from the last completed step instead of starting
over; failures are handled explicitly — retry, route to a fallback branch, or
release held resources on teardown — rather than silently lost. The same
workflow can hand a form to a person, launch a job on a Slurm or Nomad cluster,
call an LLM or tool-using agent, run a container, or drive an instrument in a
lab — and capture the resulting data into a searchable catalogue with full
provenance.

## What it does

- **Durable by default.** Execution state is event-sourced and persisted
  continuously, so long-running processes survive worker crashes, node
  preemption, network partitions, and control-plane restarts — they resume from
  the last completed step instead of starting over.
- **Advanced failure handling.** Per-step retry policies, explicit error ports
  that route to fallback / compensation branches, and finalizers that release
  held resources (a cluster allocation, a lab instrument) even when a run is
  torn down — so unpredictable environments don't strand work or leak capacity.
- **Author in an editor *or* from Git.** Build workflows in a real-time
  collaborative visual canvas (Svelte + [xyflow](https://github.com/xyflow/xyflow),
  Yjs co-editing), **or** author them as files and deploy with a rich CLI —
  `mekhan pull / diff / apply` is a GitOps flow that stamps git provenance on
  every version, and the file-first shape makes LLM-assisted workflow
  development and debugging natural.
- **Run heterogeneous work.** One graph mixes human tasks, automated steps
  (Python, containers, HTTP, SQL, ROS, …), and LLM / agent nodes across every
  execution target below — people, HPC clusters, elastic pools, and edge runners.
- **Capture & manage data.** Built-in file-metadata extraction and a searchable,
  workspace-scoped **data catalogue** over S3-backed artifacts — outputs become
  first-class, queryable data, not loose files.
- **Provenance & reproducibility.** Every run has an auditable causality trail
  you can inspect and replay — wired into the same event log that makes
  execution durable.

## Execution targets & feature highlights

One workflow can mix all of these — the platform is a single plane over them:

- **Human-in-the-loop & SOPs.** First-class human tasks with rich forms for data
  capture, structured reporting, and sign-offs — the building blocks of digital
  SOPs. Route work to **human operator pools**: tasks are offered to eligible
  members by capability matching, operators claim what they can do, and the whole
  handshake is engine-authoritative with the inbox as a live projection.
- **Datacenter scheduling.** Run steps on **Slurm and Nomad** clusters as
  first-class targets — with secure access handling (per-job, single-use secret
  tokens; credentials never live on the node), **container staging** (materialize
  an OCI image to an Apptainer `.sif` and run the executor inside it on HPC), and
  state reconciliation that detects drift between what the engine expects and the
  cluster's actual allocations.
- **Elastic worker pools.** Pull-based, queue-fed pools of interchangeable
  workers for high-throughput, low-overhead jobs — the FaaS-style side of the
  platform. They scale with load and run tasks (Python, containers, HTTP, …)
  without per-job scheduler overhead, ideal when you have many small units of
  work rather than a few large allocations.
- **Targeted runners.** Enroll a specific machine — a lab control computer, a
  workstation, an edge box — as a push-consumer runner with capability matching
  (similar in spirit to GitLab runners). **Zero-secret enrollment** means a
  runner needs only a token and a URL to come online; the simplest path for most
  small setups.
- **Capacity pools.** Model any contended, counted resource as a capacity pool —
  concurrency limits, instrument time, or **third-party floating licenses** — so
  the engine only dispatches work when a slot is genuinely free and releases it
  (even on failure) when the work is done.
- **Local LLM serving.** A self-hosted model pool behind an industry-standard,
  **OpenAI-compatible** serving API: model **autoscaling and eviction**, load
  balancing across replicas, admission control, usage metering, and model
  lifecycle management — so `llm` and agent steps run against your own
  infrastructure.

## Quick start

The fastest path to a running full stack (infra + engine + executor + control
plane + frontend) is the `just` recipe — it wires up Postgres, NATS, S3, Vault,
and seeds the demo workflows for you:

```bash
just dev                          # full stack up (see `just` for all recipes)
# → frontend  http://localhost:15173
# → API       http://localhost:13100
just dev down                     # stop everything
```

> **▶ First run:** the initial `just dev` compiles the whole Rust workspace and
> frontend from source — expect several minutes on a cold cache; later runs are
> fast. Once it's up, open the frontend (you're auto-signed-in as a dev admin),
> open the **Demos** folder, and run **`01-hello-world`** — then walk the
> numbered `01 → 06` learning path, which covers every editor primitive. See
> [`demos/README.md`](./demos/README.md) for the full catalogue.

Or run the pieces by hand:

```bash
docker compose up -d              # Postgres + NATS
cd service && cargo run           # backend
cd app && pnpm install && pnpm dev   # frontend (separate terminal)
```

Native build deps (HDF5, NetCDF, protobuf, etc.) and per-OS install one-liners
are in [`docs/setup.md`](./docs/setup.md). Nix users: `nix develop` gives you
everything.

## What's here

| Directory | What it is |
|-----------|-----------|
| [`engine/`](./engine/) | Durable, event-sourced workflow execution engine, SDK, CLI, simulator — NATS-streamed, with Slurm/Nomad bridges (Apache-2.0) |
| [`executor/`](./executor/) | Distributed task executor — Python / Docker / HTTP / LLM / ROS / … backends (Apache-2.0) |
| [`service/`](./service/) | Control plane / BFF — Axum + Postgres + NATS + Yjs; the workflow compiler, catalogue, collaboration server, and the `mekhan` GitOps CLI (FSL-1.1-ALv2) |
| [`app/`](./app/) | SvelteKit frontend — Svelte 5, xyflow canvas, Yjs collaborative editing (FSL-1.1-ALv2) |
| [`shared/`](./shared/) | Vendored `apalis` fork, file-metadata extraction, secrets plumbing |
| [`demos/`](./demos/) | 80+ runnable demo workflows, seeded automatically by `just dev` |
| [`docs/`](./docs/) | Architecture & design notes — start at [`docs/README.md`](./docs/README.md) |

For the high-level architecture and how the pieces talk to each other, see
[`CLAUDE.md`](./CLAUDE.md) and [`docs/README.md`](./docs/README.md).

## ⚠️ Security & maturity

This is an **early alpha** shared for open development. Read before deploying:

- **The dev stack is insecure by design.** `just dev` and `docker compose`
  ship with throwaway defaults — a dev Vault with root token `root`, a no-op
  auth mode where every request is a fixed admin user, and default object-store
  credentials. These exist so you can try the platform offline in one command.
  **Do not expose a dev-default deployment to the internet or put real data in
  it.** Production hardening (real auth, secret management, TLS, tenancy
  isolation) is in active development and not yet documented as turnkey.
- **No security guarantees yet.** Treat self-hosted instances as experimental.
- **Reporting a vulnerability:** please see [`SECURITY.md`](./SECURITY.md).
  Do not open public issues for security problems.

## Licensing

Multi-licensed per crate. **Open-source engine & SDK (Apache-2.0)**,
**source-available control plane** (FSL-1.1-ALv2, converts to Apache-2.0 two
years after each release). See [`LICENSING.md`](./LICENSING.md) for the
per-crate table and the rationale.

## Contributing

See [`CONTRIBUTING.md`](./CONTRIBUTING.md). Contributions go in under
inbound=outbound license with a DCO sign-off (`git commit -s`).
