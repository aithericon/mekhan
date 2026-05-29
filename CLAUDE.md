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
- mekhan-service → http://localhost:13100
- SvelteKit dev → http://localhost:15173 (proxies `/api/*` to mekhan, `/petri/*` to engine; `/api/yjs` is WS)
- engine → http://localhost:13030
- Postgres → `localhost:15439` (`mekhan:mekhan@.../mekhan`)
- NATS → `nats://localhost:14333` (HTTP monitor :18333)
- rustfs (S3) → http://localhost:19005 (admin `rustfsadmin/rustfsadmin`, console :19006)
- vault → http://localhost:18200 (dev mode, root token `root`, KV v2 at `secret/`)
- executor cancel → http://localhost:13105

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

## API surface conventions

- **`/api/v1/*` is the JSON contract.** Every `#[utoipa::path]` handler hard-codes the version in its path attribute. Bumping requires either side-by-side mounting at `/api/v2/*` or a coordinated client/server cut — there is no implicit "latest" alias.
- **`/healthz` is the liveness probe.** Lives at the root, OUTSIDE the auth gate, k8s-conventional. Load balancers and uptime monitors poll it without a session cookie. Mounted via `build_public_openapi_router` in `service/src/lib.rs`.
- **Unversioned siblings exist on purpose** because they have external contracts mekhan doesn't control:
  - `/api/auth/{login,callback,session,logout}` — OAuth bootstrap; the callback URL is registered with Zitadel.
  - `/api/yjs/{template_id}` — Yjs CRDT WebSocket (binary protocol, not OpenAPI-modeled).
  - `/api/triggers/webhook/{slug}` — webhook receivers; external senders post here.
- **`/petri/*` is the engine reverse proxy** mounted INSIDE the auth gate. Streams request + response bodies (`reqwest::Body::wrap_stream` → `axum::body::Body::from_stream`) so SSE survives, strips hop-by-hop headers per RFC 7230, and inherits the same session-cookie auth as every other API route. Forwards to `config.petri_lab_url` (default `http://localhost:13030`).

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
┌──────────┐  /api/v1/* (HTTP, OpenAPI) ┌──────────────┐   /petri/* (HTTP, proxied)  ┌────────────┐
│ SvelteKit│ ──────────────────────────▶│ mekhan-service├──────────────────────────▶ │ core-engine │
│  :5173   │  /api/yjs/* (WS, CRDT)     │     :3100     │                            │   :3030     │
│          │  /healthz (LB probe)       │               │                            │             │
└──────────┘ ◀───────────────────────── └──────────────┘ ◀── NATS (jetstream) ──────────────────────┘
                                                │                                     ▲
                                                ▼                                     │
                                      Postgres + S3 (rustfs)              NATS ──▶ executor (daemon)
```

The SPA goes through mekhan for ALL backend traffic — JSON API, Yjs CRDT WS,
AND engine calls. `/petri/*` is a reverse proxy inside mekhan (streaming
bodies, hop-by-hop header strip) so prod can run single-origin without a
separate engine ingress, and dev keeps parity by routing through mekhan
instead of Vite's old direct-to-engine rewrite.

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
- **Dev Vault is in-memory and ephemeral.** `mekhan-vault` runs in `-dev` mode with root token `root` and no persistence — every `just dev down`/`reset` wipes resource secrets (the Postgres `resources` rows survive, but `resource_versions.vault_path` will then point at empty Vault entries). `_infra-wipe` has no vault volume to scrub for the same reason. Resource CRUD flow: mekhan writes to `secret/data/aithericon/resources/{workspace}/{resource}/v{version}` via `VaultResourceStore` (auto-selected when `VAULT_ADDR`/`VAULT_TOKEN` are set); engine wraps secrets into single-use tokens at job-submit; executor unwraps with `VAULT_ADDR` alone.
- **Worktree builds** that touch engine/executor/shared sibling crates resolve workspaces upward through `.claude/worktrees/...`; that's why the umbrella's `exclude` list contains `.claude`. Don't remove it.
- **Worktrees use sccache, not shared target dirs.** Earlier this repo shared `CARGO_TARGET_DIR=$HOME/.cache/cargo-targets/aithericon-platform/{umbrella,engine,executor}/` across worktrees to save disk, but cargo's per-target-dir lock then serialized concurrent builds — two worktrees calling `cargo build` (or one calling `cargo build` while the other's rust-analyzer is checking) would block each other, killing parallel work. The current setup uses **sccache as `RUSTC_WRAPPER`** instead: per-worktree `target/` dirs are restored (cargo's default — no lock contention), but compiled crates are content-addressed in `~/Library/Caches/Mozilla.sccache` so different worktrees don't recompile the same deps. The `.envrc` files (umbrella, `engine/`, `executor/`) — tracked — export `RUSTC_WRAPPER=sccache`, `SCCACHE_CACHE_SIZE=100G`, and `CARGO_INCREMENTAL=0` via direnv. `CARGO_INCREMENTAL=0` is required: sccache refuses to cache incremental-mode artifacts. Trade-off: edit-loop builds on a single crate lose incremental compilation, but cold + post-`cargo clean` rebuilds get cached across worktrees, which is the actual pain point with multiple worktrees. Disk math: ~80 GB per worktree × N + up to ~100 GB sccache cache. `just dev::worktree-add <name>` creates a worktree and runs `just/scripts/setup-cargo-cache.sh` (idempotent — writes any missing `.envrc` files, strips legacy shared-target blocks, and `direnv allow`s all three). `just dev::gc-targets` runs `cargo-sweep` across the three workspaces to GC artifacts older than 30 days (override with `gc-targets 7`). To clean up the legacy shared dirs from the prior scheme, run `just dev::setup-cargo-cache --clean-shared` once. Verify hits with `sccache --show-stats`. The tracked `.cargo/config.toml` is unchanged (still owns the cross-musl linker setup).
- **Per-worktree dev stacks.** `just dev` is no longer fixed-port: each worktree gets a private host-port block + docker compose project so two worktrees can run `just dev` at once without colliding on ports, container names, or volumes. The mechanism is one knob — a **slot** integer stored in the gitignored `.dev/slot` (absent → slot 0 = the historical fixed ports, the main checkout). `just/scripts/dev-ports.sh` is the single source of truth: it maps the slot to every port (`slot N` → a 100-wide block at `20000 + N*100`; e.g. slot 3 → mekhan `20300`, engine `20301`, pg `20310`, …) plus `COMPOSE_PROJECT_NAME=mekhan-s<N>`, and `export`s them. The tracked `.envrc` sources it (managed block, identical across worktrees — safe to commit; the per-worktree number lives only in `.dev/slot`). Everything downstream reads that env: `docker-compose.yml` interpolates host ports with `${VAR:-legacy}` fallbacks and pins **no** `container_name` (so the project prefix namespaces containers/volumes/networks); `just/{dev,db}.just` read the ports via `env_var_or_default(...)`; `app/vite.config.ts` reads `MEKHAN_APP_PORT`/`MEKHAN_SERVICE_URL`. `just dev::worktree-add` auto-assigns the lowest free slot ≥ 1 and writes `.dev/slot`; `just dev::setup-cargo-cache --slot N` (re)assigns one for an existing worktree (then `direnv reload`). Ollama (`11434`) is intentionally shared across worktrees — model downloads are heavy and serving is read-only. Existing worktrees created before this change default to slot 0 (they collide, as before) until you assign them a slot.
- **Rust-analyzer phantom errors after env changes.** rust-analyzer spawned by Claude Code (or any IDE/LSP) inherits the **parent process's** env, not direnv's — so a long-lived LSP started before an `.envrc` change keeps the stale env (e.g. an old `CARGO_TARGET_DIR` from the prior shared-target scheme) and reports phantom errors against stale artifacts even though `cargo check` (running with direnv-loaded env) is clean. Fix in `.claude/settings.local.json` via `env.RUSTC_WRAPPER`/`env.CARGO_INCREMENTAL` for new Claude sessions; for the running session, (a) verify with `direnv exec . cargo check`, (b) restart the Claude session (or `kill <rust-analyzer pid>` to force respawn under the new env). Note that `.claude/settings.local.json` is gitignored, so the values baked in there are per-machine.

## Reference

- Top-level architecture & migration docs: `docs/` (numbered 01–10 + setup/authoring/README).
- Engine: `engine/CLAUDE.md` + `engine/docs/`
- Executor: `executor/CLAUDE.md` + `executor/docs/`
- Per-OS native deps (HDF5/NetCDF/protoc/cmake): `docs/setup.md`. Nix users: `nix develop`.
- Licensing is multi-tier per crate (Apache-2.0 for engine/SDK/executor, FSL-1.1-ALv2 for `service` + `app`). See `LICENSING.md`. Contributions require DCO sign-off (`git commit -s`); see `CONTRIBUTING.md`.
