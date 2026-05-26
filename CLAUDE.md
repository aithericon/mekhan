# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repo shape

Monorepo with **four logically independent workspaces** glued together by a thin umbrella `Cargo.toml`:

| Path | Workspace | What it is |
|------|-----------|-----------|
| `service/` | umbrella root (`./Cargo.toml`) | `mekhan-service` — Axum BFF + control plane (Postgres, NATS, Yjs, S3). The umbrella's only Cargo member. |
| `engine/` | own root | Petri-net execution engine (`core-engine`, SDK, CLI `aithericon`). Has its own `CLAUDE.md`. |
| `executor/` | own root | Distributed task executor (Python / Docker / HTTP backends, NATS-driven). Has its own `CLAUDE.md`. |
| `app/` | n/a (SvelteKit) | Svelte 5 + xyflow + Yjs frontend. Talks to mekhan-service via OpenAPI-typed client. |
| `shared/` | own roots | Vendored `apalis` fork, `aithericon-file-metadata`, secrets plumbing. |

The umbrella `excludes` `app`, `engine`, `executor`, `shared/*`, and `.claude` (worktrees). Path-deps still reach them. This shape exists so `cross` can mount one root for musl cross-builds and so worktrees don't get captured by the wrong workspace.

When working on `engine/` or `executor/` internals, **read the nested `CLAUDE.md` in that subdirectory first** — they own their own build/test recipes and architecture notes.

## Build & run — local dev

Everything goes through `just`. The justfile is modular: top-level `justfile` imports `just/{dev,ci,db}.just`. Note that `just` modules execute with CWD = the module file; shebang recipes therefore start with `cd ..` to operate from repo root.

```bash
just                       # list all recipes
just dev                   # full stack up: infra + executor + engine + mekhan + app
just dev down              # stop everything
just dev status            # what's running (pidfiles in .dev/pid/, logs in .dev/log/)
just dev logs <name>       # tail one of: executor | engine | mekhan | app | infra
just dev restart           # down + up (data preserved)
just dev reset             # down + WIPE pg/nats/rustfs volumes + up (zitadel survives)
just dev psql              # psql into local DB
just dev openapi           # regenerate openapi-mekhan.json + app/src/lib/api/schema.d.ts
just dev up-<name>         # single-component restart: up-executor | up-engine | up-mekhan | up-app
```

Local endpoints once `just dev` is up:
- mekhan-service → http://localhost:3100
- SvelteKit dev → http://localhost:5173 (proxies `/api/*` to mekhan, `/petri/*` to engine; `/api/yjs` is WS)
- engine → http://localhost:3030
- Postgres → `localhost:5439` (`mekhan:mekhan@.../mekhan`)
- NATS → `nats://localhost:4333` (HTTP monitor :8333)
- rustfs (S3) → http://localhost:9005 (admin `rustfsadmin/rustfsadmin`, console :9006)
- executor cancel → http://localhost:3105

Auth defaults to `dev_noop` (every request is a fixed dev user, fully offline). Use `just dev up-auth` to run mekhan in BFF mode against Zitadel — requires `bash deploy/zitadel/bootstrap.sh` once to write `service/mekhan.local.toml`.

### Scheduler layers

Default `just dev` cannot run `Scheduled` AutomatedSteps — there's no `SCHEDULER_BACKEND` set. To exercise that path end-to-end:

```bash
just dev scheduler-up      # Nomad-backed (needs `nomad` on PATH)
just dev slurm-up          # Slurm-backed (needs cargo-zigbuild + Docker; cross-compiles executor)
just dev scheduler-down    # / slurm-down — return engine to plain dev
```

While a scheduler layer is up, the dev executor daemon is paused and **inline AutomatedSteps will not run** — the Nomad/Slurm worker is the sole consumer of the `executor` queue.

## Build & test — CI parity

CI recipes in `just/ci.just` are the single source of truth for `.woodpecker/*.yml`. Run them locally to mirror CI:

```bash
just ci::quality-rust         # cargo fmt --check + cargo clippy --workspace -- -D warnings
just ci::quality-frontend     # pnpm install --frozen-lockfile && pnpm run check  (svelte-check)
just ci::check-rust           # cargo check --workspace --lib --bins
just ci::test-rust            # cargo test --workspace --no-fail-fast
just ci::test-frontend-unit   # vitest
just ci::test-frontend-e2e    # playwright (needs an app already built + served)
just ci::openapi-drift        # regen openapi + schema; fail if diff vs. committed
just ci::build-all            # quality → test → cross-build (aarch64-musl) → docker package
```

### Running a single test

```bash
# Rust — mekhan-service is built from the UMBRELLA root, so output lands in ./target/
cargo test --workspace <test_name>
cargo test -p mekhan-service --test compiler_e2e -- <pattern>

# Engine / executor — use the nested workspace
(cd engine   && cargo test -p petri-application <pattern>)
(cd executor && cargo test -p aithericon-executor-service --test integration -- <pattern>)

# Frontend
(cd app && pnpm exec vitest run path/to/file.test.ts)
(cd app && pnpm exec playwright test tests/e2e/<spec>.test.ts)
```

`mekhan-service` integration tests under `service/tests/` mostly need a live local stack (`just dev`). Heavy / Docker-dependent gated lanes (`MEKHAN_E2E_ZITADEL=1`, `TEST_S3_BUCKET=...`) are documented inline in the test files and in `just ci::test-e2e-zitadel`.

## OpenAPI is a hard contract

The frontend client is generated from `openapi-mekhan.json`. After **any** change to a Rust `#[utoipa::path]` handler, `ToSchema`-derived DTO, or `IntoParams` query type, you MUST regenerate:

```bash
just dev::openapi
# = cargo run --bin mekhan -- openapi > openapi-mekhan.json
#   && (cd app && pnpm openapi:generate)
```

`just ci::openapi-drift` enforces this in CI — a PR with stale `openapi-mekhan.json` or `app/src/lib/api/schema.d.ts` fails the gate.

## Architecture — the load-bearing pieces

### Three Rust services, one frontend

```
┌──────────┐  /api (HTTP, OpenAPI)   ┌──────────────┐   /petri (HTTP)   ┌────────────┐
│ SvelteKit│ ──────────────────────▶ │ mekhan-service│ ────────────────▶ │ core-engine │
│  :5173   │  /api/yjs (WS, CRDT)   │     :3100     │                   │   :3030     │
└──────────┘ ◀─────────────────────  └──────────────┘ ◀── NATS (jetstream) ──────────┘
                                              │                              ▲
                                              ▼                              │
                                    Postgres + S3 (rustfs)         NATS ──▶ executor (daemon)
```

- **mekhan-service** is the BFF + control plane. Owns templates/instances/triggers/auth/files/catalogue/causality projections + the Yjs collaboration server. Compiles `WorkflowGraph` → AIR JSON and POSTs to the engine for deploy/activate. Lives in `service/src/`.
- **core-engine** (in `engine/`) is the Petri-net executor: event-sourced, NATS-streamed, with bridges to Nomad/Slurm. Don't reimplement engine concerns in `service` — `service` is a compiler + control plane on top of it.
- **executor** (in `executor/`) is the long-running job worker. mekhan and the engine never talk to it directly — they enqueue via NATS, executor pulls work, publishes status updates back over NATS.
- **app** is a Svelte 5 SPA. Visual graph editor uses `@xyflow/svelte`; collaborative editing uses Yjs over the BFF.

### Service-side compiler (`service/src/compiler/`)

mekhan's compiler is **the borrow-checker** for the platform's typed-port / control-data model (see `docs/10-control-data-token-model.md`):

- Every node's business output is **parked write-once** in a `p_{id}_data` place. Only a slim **control token** moves by-value through the net.
- Guards / loop conditions / End mappings that need an upstream field get a non-consuming **read-arc** synthesized into the parked producer place.
- References are **producer-namespaced**: `<slug>.<field>`. `slug` is the Rhai-identifier-safe key on `WorkflowNode.slug`. Two nodes with the same explicit slug → `CompileError::SlugConflict`.
- `input.<path>` is reserved for genuinely control-token-resident leaves (Start fields, `_loop_*`, `task_id`, `status`) — attributed to a synthetic "Process" group.
- A **single resolver** feeds the editor variable picker, diagnostics, and read-arc synthesis. They cannot drift.

Pipeline phases live in `compiler/{graph,validate,lower,wire,compile,subworkflow,pyio,rhai_gen,...}.rs`. The runtime schema layer (`SchemaRegistry` in `petri-application`) enforces the `definitions` the compiler emits.

### Frontend ↔ backend type plumbing

- `app/src/lib/api/schema.d.ts` is generated from `openapi-mekhan.json` via `openapi-typescript`. **Never hand-edit.**
- `app/src/lib/api/client.ts` wraps `openapi-fetch` against that schema.
- Stale TS errors on `schema.d.ts`-derived types from the LSP popup can be misleading after a regen — trust `(cd app && npx svelte-check)`.

### Demos

`demos/<name>/` directories double as bundled fixtures and as publishable templates — the same on-disk shape as the GitOps `mekhan pull/apply` flow. `MEKHAN__DEMOS__SEED=true` (set in `just dev`) makes mekhan-service seed them on startup from `MEKHAN__DEMOS__DIR`. `mekhan_service::demos::load_demo()` is the loader; see `demos/README.md`.

## Things that look weird but are intentional

- **mekhan-service builds from the umbrella root**, not from `service/`. Output is `./target/debug/mekhan-service`, not `service/target/...`. Any recipe that touches the binary path must respect this. `engine/` and `executor/` are the opposite — they build into `engine/target/` and `executor/target/`.
- **`cargo fmt`** locally may disagree with what CI's nix-pinned toolchain enforces. Don't auto-`cargo fmt` to "fix" CI — fix the specific line CI flagged.
- **mekhan-service and aithericon-executor-service are separate binaries in separate workspaces.** A change to a runner trait used by both needs BOTH rebuilt + restarted + republished before live dev reflects it.
- **The dev NATS is disposable.** `just dev reset` wipes pg/nats/rustfs volumes — both the `nats data` *volume* and the container restart (just restarting the container without removing `mekhan-natsdata` keeps stale JetStream state).
- **Worktree builds** that touch engine/executor/shared sibling crates resolve workspaces upward through `.claude/worktrees/...`; that's why the umbrella's `exclude` list contains `.claude`. Don't remove it.

## Reference

- Top-level architecture & migration docs: `docs/` (numbered 01–10 + setup/authoring/README).
- Engine: `engine/CLAUDE.md` + `engine/docs/`
- Executor: `executor/CLAUDE.md` + `executor/docs/`
- Per-OS native deps (HDF5/NetCDF/protoc/cmake): `docs/setup.md`. Nix users: `nix develop`.
- Licensing is multi-tier per crate (Apache-2.0 for engine/SDK/executor, FSL-1.1-ALv2 for `service` + `app`). See `LICENSING.md`. Contributions require DCO sign-off (`git commit -s`); see `CONTRIBUTING.md`.
