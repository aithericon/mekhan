# =============================================================================
# Local dev environment
# =============================================================================
# One-stop dev stack: infra (postgres + nats + rustfs) + native daemons
# (aithericon-executor, engine core-engine, mekhan-service, mekhan-app).
#
#   just dev              # = `just dev up`, idempotent
#   just dev down         # stop everything
#   just dev logs <name>  # tail one component
#   just dev status       # what's running
#   just dev restart      # down + up (data preserved)
#   just dev reset        # down + wipe pg/nats/rustfs volumes + up
#
# Per-component recipes (`up-executor`, `up-engine`, …) restart a single
# service without touching the rest — useful when iterating.
#
# Native daemons run via `nohup` with pidfiles in `.dev/pid/` and logs in
# `.dev/log/`. Cargo builds are incremental, so re-`up`ing is sub-second
# after the first build.
# =============================================================================

set shell := ["bash", "-euo", "pipefail", "-c"]
set working-directory := ".."

dev_dir   := ".dev"
pid_dir   := ".dev/pid"
log_dir   := ".dev/log"
work_dir  := ".dev/work"
nomad_jobs_dir := ".dev/nomad-jobs"

# Executor build features. `llm` is included so an AutomatedStep with
# backendType=llm can dispatch against the local Ollama daemon
# (`just dev ollama-up`); external-provider workflows need API keys in the
# LlmConfig but the local Ollama path needs none. `kreuzberg` is
# included so demo 07 (OCR + LLM classification) works out of the box —
# first build pulls heavy pdfium deps, but the cached rebuild cost is
# normal.
executor_features := "docker,python,http,http-cancel,file-ops,opendal-s3,url-inputs,llm,kreuzberg,smtp,postgres,loki,vault"

# Local infra + daemon endpoints. These read the per-worktree env exported by
# just/scripts/dev-ports.sh (via direnv / .envrc), falling back to the historical
# fixed ports when unset (slot 0 / no direnv / CI). This is the single knob that
# lets concurrent worktrees run isolated `just dev` stacks — see CLAUDE.md
# "Per-worktree dev stacks". Keep these in sync with dev-ports.sh's defaults.
nats_url     := env_var_or_default("MEKHAN_NATS_URL", "nats://localhost:14333")
nats_mon_url := env_var_or_default("MEKHAN_NATS_MON_URL", "http://localhost:18333")
database_url := env_var_or_default("MEKHAN_DATABASE_URL", "postgres://mekhan:mekhan@localhost:15439/mekhan")
s3_endpoint  := env_var_or_default("MEKHAN_S3_ENDPOINT", "http://localhost:19005")
vault_addr   := env_var_or_default("MEKHAN_VAULT_ADDR", "http://localhost:18200")
vault_token  := "root"
service_url  := env_var_or_default("MEKHAN_SERVICE_URL", "http://localhost:13100")
engine_url   := env_var_or_default("MEKHAN_ENGINE_URL", "http://localhost:13030")
service_port := env_var_or_default("MEKHAN_SERVICE_PORT", "13100")
engine_port  := env_var_or_default("MEKHAN_ENGINE_PORT", "13030")
cancel_port  := env_var_or_default("MEKHAN_CANCEL_PORT", "13105")
app_port     := env_var_or_default("MEKHAN_APP_PORT", "15173")
s3_console_port := env_var_or_default("MEKHAN_S3_CONSOLE_PORT", "19006")
nats_port    := env_var_or_default("MEKHAN_NATS_PORT", "14333")
s3_port      := env_var_or_default("MEKHAN_S3_PORT", "19005")
mailpit_smtp_port := env_var_or_default("MEKHAN_MAILPIT_SMTP_PORT", "1025")
mailpit_ui_port   := env_var_or_default("MEKHAN_MAILPIT_UI_PORT", "8025")
httpbin_port := env_var_or_default("MEKHAN_HTTPBIN_PORT", "13110")

# ── Entrypoints ──────────────────────────────────────────────────────────────

# Default recipe: bring up the full stack
default: up

# Start everything: infra + ollama + executor + engine + mekhan + app
# (auth: dev_noop). Ollama starts BEFORE the executor so the first
# LLM-step dispatch finds a ready server. If Ollama isn't installed
# locally, `up-ollama` no-ops with a hint — the rest of the stack still
# comes up; only LLM-using workflows would fail at dispatch time.
up:
    @just dev::_dirs
    @just dev::_infra-up
    @just dev::_wait-infra
    @just dev::up-ollama
    @just dev::up-executor
    @just dev::up-engine
    @just dev::_wait-engine-stream
    @just dev::up-mekhan
    @just dev::up-app
    @just dev::_summary

# Same as `up` but with auth ENABLED: mekhan-service runs in BFF mode (real
# Zitadel cookie login for the SPA + machine-PAT introspection for the CLI).
# Requires `bash deploy/zitadel/bootstrap.sh` once (writes mekhan.local.toml).
up-auth:
    @just dev::_dirs
    @just dev::_infra-up
    @just dev::_wait-infra
    @just dev::up-ollama
    @just dev::up-executor
    @just dev::up-engine
    @just dev::_wait-engine-stream
    @just dev::up-mekhan bff
    @just dev::up-app
    @just dev::_summary "ENABLED (BFF + Zitadel PAT introspection)"

# Stop everything: native daemons first, then infra
down:
    @just dev::down-app      || true
    @just dev::down-mekhan   || true
    @just dev::down-engine   || true
    @just dev::down-executor || true
    @just dev::down-ollama   || true
    @just dev::_kill nomad   || true
    @just dev::_infra-down

# Restart the whole stack
restart:
    @just dev::down
    @just dev::up

# Tears the whole stack down, destroys the postgres + nats + rustfs data
# volumes, then brings everything back up clean. The native daemons re-run
# sqlx migrations / recreate the jetstream + S3 bucket on boot, so this
# yields a pristine environment. Zitadel (zitadel-pgdata) is intentionally
# left untouched so its bootstrap survives.
#
# Complete wipe of pg/nats/rustfs, then back up clean
reset:
    @just dev::down
    @just dev::_infra-wipe
    @just dev::up

# Show what's running
status:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "── infra ──────────────────────────────────────────────"
    docker compose ps --format "table {{ '{{.Name}}' }}\t{{ '{{.Status}}' }}\t{{ '{{.Ports}}' }}" 2>/dev/null || echo "  (compose not running)"
    echo ""
    echo "── native daemons ─────────────────────────────────────"
    for name in ollama executor engine nomad mekhan app; do
      pf="{{pid_dir}}/$name.pid"
      if [ -s "$pf" ] && kill -0 "$(cat "$pf")" 2>/dev/null; then
        printf "  %-10s up  (pid %s)\n" "$name" "$(cat "$pf")"
      else
        printf "  %-10s down\n" "$name"
      fi
    done

# Tail one component's log (executor | engine | mekhan | app | infra)
logs name:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "{{name}}" = "infra" ]; then
      docker compose logs -f
    else
      f="{{log_dir}}/{{name}}.log"
      if [ ! -f "$f" ]; then echo "no log at $f"; exit 1; fi
      tail -F "$f"
    fi

# Drop into a psql shell against the local DB
psql:
    psql "{{database_url}}"

# Configure this worktree to use sccache as cargo's rustc wrapper, so
# compiled crates are content-addressed in ~/Library/Caches/Mozilla.sccache
# and reused across worktrees without sharing target/ dirs (which would
# serialize concurrent builds via cargo's target-dir lock). Pass
# --clean-shared to additionally delete the legacy
# ~/.cache/cargo-targets/aithericon-platform/ dirs from the old shared-target
# scheme. Safe to re-run. Normally only needed if .envrc files are missing —
# fresh worktrees created via `just dev::worktree-add` already have them.
setup-cargo-cache *args:
    just/scripts/setup-cargo-cache.sh {{args}}

# GC stale build artifacts across all three workspaces. cargo-sweep removes
# target/ files older than N days (default 30). Run periodically if disk
# fills up — per-worktree target/ dirs can grow large even with sccache
# (sccache caches rustc outputs but cargo still writes them to target/).
# Use --installed instead of --time to prune artifacts from other toolchains.
gc-targets days="30":
    #!/usr/bin/env bash
    set -euo pipefail
    command -v cargo-sweep >/dev/null 2>&1 || {
      echo "✗ cargo-sweep not on PATH — brew install cargo-sweep" >&2; exit 1; }
    for ws in . engine executor; do
      echo "▶ sweeping $ws/target (older than {{days}} days)…"
      (cd "$ws" && cargo sweep --time {{days}} || true)
    done
    echo
    echo "sccache stats:"
    sccache --show-stats || true

# Create a worktree at .claude/worktrees/<name> with sccache pre-allowed.
# Trailing args pass through to `git worktree add` (e.g. a start-point, or
# `-b <new-branch>`). The tracked .envrc files come with the checkout;
# this recipe runs `direnv allow` for the three workspaces so the first
# `cargo` invocation already has RUSTC_WRAPPER=sccache wired up.
#
# Examples:
#   just dev::worktree-add foo                       # branch off HEAD, name `foo`
#   just dev::worktree-add foo main                  # start at main
#   just dev::worktree-add foo -b refactor/bar main  # new branch `refactor/bar` from main
worktree-add name *args:
    #!/usr/bin/env bash
    set -euo pipefail
    wt=".claude/worktrees/{{name}}"
    if [[ -e "$wt" ]]; then
        echo "✗ $wt already exists" >&2
        exit 1
    fi
    # Capture script path BEFORE creating the worktree — if the script
    # isn't yet committed, the new worktree won't contain it, so we invoke
    # the main repo's copy by absolute path.
    setup_script="$(pwd)/just/scripts/setup-cargo-cache.sh"
    # Allocate the lowest free dev-stack slot ≥ 1 (slot 0 = the main checkout).
    # Each worktree records its slot in the gitignored .dev/slot file; scan
    # them all so two worktrees never share a slot (= never collide on
    # ports/compose). Reads happen from the main repo root (this recipe's CWD).
    used="$( { cat .dev/slot .claude/worktrees/*/.dev/slot ; } 2>/dev/null | grep -oE '^[0-9]+$' | sort -nu || true)"
    slot=1
    while printf '%s\n' "$used" | grep -qx "$slot"; do slot=$((slot + 1)); done
    echo "▶ assigning dev-stack slot $slot to {{name}}"
    git worktree add "$wt" {{args}}
    # Run the setup script inside the new worktree. Idempotent: writes the
    # sccache + dev-env (WORKTREE_SLOT) .envrc blocks and direnv-allows them, so
    # the new worktree picks up RUSTC_WRAPPER=sccache + its private port block on
    # first cd.
    (cd "$wt" && "$setup_script" --slot "$slot")
    echo
    echo "✓ worktree ready: $wt   (dev slot $slot)"
    echo "  cd $wt   # direnv loads sccache + WORKTREE_SLOT=$slot automatically"
    echo "  just dev # brings up this worktree's isolated stack (ports: 200${slot}x range)"

# Regenerate openapi-mekhan.json (from the mekhan-service binary, no infra
# needed) and refresh the SvelteKit app's typed schema from it. Run this
# after any change to a #[utoipa::path] annotation or ToSchema-derived DTO.
#
# Uses `cargo run` rather than a hardcoded `./target/debug/mekhan` so the
# target dir resolves through cargo's normal chain (CARGO_TARGET_DIR env →
# .cargo/config.toml → ./target). Worktrees with shared CARGO_TARGET_DIR
# (see CLAUDE.md "Worktrees share cargo target dirs by default") would
# otherwise either fail (no such file) or — worse — invoke a stale local
# ./target/debug/mekhan from a prior non-shared build. Cargo's `Compiling
# …` / `Finished` lines go to stderr, so the stdout redirect stays clean.
openapi:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "▶ dumping spec → openapi-mekhan.json (cargo run --bin mekhan)"
    cargo run --quiet --bin mekhan -- openapi > openapi-mekhan.json
    echo "▶ regenerating app/src/lib/api/schema.d.ts"
    (cd app && pnpm run openapi:generate)
    echo "✓ openapi spec + schema regenerated"

# Remove every seeded demo workflow (cancels running instances + purges nets).
# Thin wrapper over `mekhan demos reset` → POST /api/v1/admin/demos/reset.
# Requires mekhan running (`just dev`); dev_noop needs no token.
demos-reset:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo run --quiet --bin mekhan -- --server {{service_url}} demos reset

# Reset seeded demos to pristine: remove every seeded family, then re-seed
# from ./demos. Overwrites any edits made via the editor. Wrapper over
# `mekhan demos reseed` → POST /api/v1/admin/demos/reseed.
demos-reseed:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo run --quiet --bin mekhan -- --server {{service_url}} demos reseed

# ── Infra ────────────────────────────────────────────────────────────────────

_infra-up:
    @echo "▶ infra up (postgres + nats + rustfs + vault)…"
    @docker compose up -d

_infra-down:
    @echo "▶ infra down…"
    @docker compose down

# Destroy the postgres + nats + rustfs data volumes. Assumes the stack is
# already down (see `reset`) so nothing holds the volumes open. Volume names
# follow docker compose's `<project>_<volume>` convention; project name
# defaults to the compose dir basename (matches just/db.just's `reset`).
_infra-wipe:
    #!/usr/bin/env bash
    set -euo pipefail
    project="${COMPOSE_PROJECT_NAME:-$(basename "$PWD")}"
    echo "▶ wiping infra data volumes (postgres + nats + rustfs)…"
    for vol in mekhan-pgdata mekhan-natsdata mekhan-rustfs-data mekhan-rustfs-logs; do
      if docker volume rm "${project}_${vol}" >/dev/null 2>&1; then
        echo "  ✓ removed ${project}_${vol}"
      else
        echo "  · ${project}_${vol} (absent or already gone)"
      fi
    done

_wait-infra:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "▶ waiting for postgres…"
    for _ in $(seq 1 30); do
      docker compose exec -T postgres pg_isready -U mekhan -d mekhan >/dev/null 2>&1 && break
      sleep 1
    done
    echo "▶ waiting for nats…"
    for _ in $(seq 1 30); do
      curl -sf "{{nats_mon_url}}/healthz" >/dev/null 2>&1 && break
      sleep 1
    done
    echo "▶ waiting for rustfs…"
    for _ in $(seq 1 30); do
      curl -sf "{{s3_endpoint}}/" >/dev/null 2>&1 && break
      sleep 1
    done
    echo "▶ waiting for vault…"
    for _ in $(seq 1 30); do
      curl -sf "{{vault_addr}}/v1/sys/health?standbyok=true" >/dev/null 2>&1 && break
      sleep 1
    done

# ── Ollama (local inference daemon, native `provider:"ollama"` adapter) ─────
#
# Stands up a local Ollama daemon the executor's LLM backend dispatches against
# (provider=ollama + base_url=http://localhost:11434 — no api_key needed). The
# Ollama adapter supports vision-capable models via the native /api/chat
# image-payload field; see executor/crates/executor-llm/src/adapters/ollama.rs.
#
# Prerequisite (per-machine, one-time): install Ollama. Either the macOS app
# (https://ollama.com/download) or the CLI:
#
#   brew install ollama
#
# The recipe defers to an already-running daemon (macOS app or external
# `ollama serve`) and only spawns its own when nothing is listening on
# OLLAMA_PORT. Model pulls are idempotent — first `up` downloads the default
# model (~7.8 GB for llama3.2-vision:11b); subsequent runs are no-ops.
#
# Knobs (env-overridable on the recipe call):
#   OLLAMA_MODEL                   — model tag (default qwen3.5:9b — direct Ollama-library equivalent
#                                    of the prior vllm-metal default `mlx-community/Qwen3.5-9B-MLX-4bit`;
#                                    ~5.5 GB GGUF Q4_K_M). Other workable picks:
#                                      qwen3.5:2b                              (~1.4 GB, lightest)
#                                      qwen3.5:27b                             (~16 GB, more capable)
#                                      qwen3-vl:8b                             (~6 GB, vision-capable variant)
#                                      llama3.2-vision:11b                     (~7.8 GB, alternative vision)
#                                    If you change OLLAMA_MODEL, also update the `model` field in any
#                                    demo whose LlmConfig pins the tag (the demo carries it explicitly;
#                                    Ollama serves under exactly the tag it loaded).
#   OLLAMA_PORT                    — port (default 11434)
#   OLLAMA_READY_TIMEOUT_SECS      — readiness poll budget (default 60; the daemon itself starts in <1s,
#                                    the budget covers the cold model pull on first run, which can take
#                                    minutes; raise for larger models on slow connections)
up-ollama:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs
    port="${OLLAMA_PORT:-11434}"
    model="${OLLAMA_MODEL:-qwen3.5:9b}"
    ready_timeout="${OLLAMA_READY_TIMEOUT_SECS:-60}"

    # Defer to an already-running daemon (macOS Ollama.app, system service,
    # or a prior `ollama serve`). We only spawn — and pidfile — when nothing
    # answers on the port.
    if curl -sf "http://localhost:${port}/api/version" >/dev/null 2>&1; then
      echo "  ollama already serving on :${port} (external — no pidfile)"
    else
      if ! command -v ollama >/dev/null 2>&1; then
        echo "  · ollama not installed — skipping"
        echo "    install:  brew install ollama   (or https://ollama.com/download for the macOS app)"
        echo "    (the rest of the dev stack still comes up; only LLM-using workflows would fail)"
        exit 0
      fi
      echo "▶ starting ollama serve on :${port}…"
      nohup env OLLAMA_HOST="127.0.0.1:${port}" ollama serve \
        > "{{log_dir}}/ollama.log" 2>&1 &
      echo $! > "{{pid_dir}}/ollama.pid"
      ready=0
      for _ in $(seq 1 "$ready_timeout"); do
        if curl -sf "http://localhost:${port}/api/version" >/dev/null 2>&1; then
          ready=1; break
        fi
        sleep 1
      done
      if [ "$ready" != "1" ]; then
        echo "✗ ollama did not become ready in ${ready_timeout}s (log: {{log_dir}}/ollama.log)" >&2
        exit 1
      fi
      echo "  ✓ ollama (pid $(cat {{pid_dir}}/ollama.pid))  http://localhost:${port}  log: {{log_dir}}/ollama.log"
    fi

    # Idempotent model pull. `ollama list` enumerates installed tags; grep for
    # the exact tag rather than substring so `:11b` doesn't also match `:11b-q8`.
    if ollama list 2>/dev/null | awk 'NR>1 {print $1}' | grep -Fxq "$model"; then
      echo "  ✓ model already present: $model"
    else
      echo "▶ pulling model: $model  (first run only; can take several minutes)"
      ollama pull "$model"
      echo "  ✓ pulled $model"
    fi

down-ollama:
    @just dev::_kill ollama

# ── MailHog (test SMTP capture server) ───────────────────────────────────────
#
# MailHog is a tiny SMTP-receiving service that captures every message and
# exposes them through an HTTP API + web UI. Used by the SMTP executor
# backend in dev/test:
#   • SMTP port:  1025   (point your `mail` resource at host=localhost port=1025)
#   • Web UI:     8025   (browse received messages)
#   • API:        8025/api/v1/messages
#
# Requires Docker (idempotent: re-running just makes the container available
# again; pre-existing containers are detected and reused).
#
# Env:
#   MAILHOG_TAG  — image tag (default `mailhog/mailhog:latest`)
mailhog-up:
    #!/usr/bin/env bash
    set -euo pipefail
    tag="${MAILHOG_TAG:-mailhog/mailhog:latest}"
    container="${COMPOSE_PROJECT_NAME:-mekhan}-mailhog"
    if docker ps --format '{{{{.Names}}}}' | grep -Fxq "$container"; then
      echo "  ✓ mailhog already running (container: $container)"
      echo "    SMTP   localhost:{{mailpit_smtp_port}}"
      echo "    UI     http://localhost:{{mailpit_ui_port}}"
      exit 0
    fi
    if docker ps -a --format '{{{{.Names}}}}' | grep -Fxq "$container"; then
      docker start "$container" >/dev/null
    else
      echo "▶ starting mailhog (image: $tag) — SMTP :{{mailpit_smtp_port}}, UI :{{mailpit_ui_port}}"
      docker run -d --rm --name "$container" \
        -p {{mailpit_smtp_port}}:1025 -p {{mailpit_ui_port}}:8025 \
        "$tag" >/dev/null
    fi
    echo "  ✓ mailhog up"
    echo "    SMTP   localhost:{{mailpit_smtp_port}}  (host=localhost port={{mailpit_smtp_port}} in your `smtp` resource)"
    echo "    UI     http://localhost:{{mailpit_ui_port}}  (received messages land here)"

mailhog-down:
    #!/usr/bin/env bash
    set -euo pipefail
    container="${COMPOSE_PROJECT_NAME:-mekhan}-mailhog"
    if docker ps --format '{{{{.Names}}}}' | grep -Fxq "$container"; then
      docker stop "$container" >/dev/null
      echo "  ✓ mailhog stopped"
    else
      echo "  · mailhog not running"
    fi

# ── httpbin (local HTTP echo server) ─────────────────────────────────────────
#
# go-httpbin is a tiny static-binary echo service. The HTTP executor backend
# (and the `11-http-call` demo) point at it so `{{ slug.field }}` borrows in a
# request URL/headers/query/body can be exercised end-to-end with no internet:
#   • Host port:  13110  → container :8080
#   • /anything/<path>   echoes method, url, args, headers, and JSON body back
#
# Requires Docker (idempotent: a pre-existing container is detected + reused).
#
# Env:
#   HTTPBIN_TAG  — image tag (default `mccutchen/go-httpbin:v2.15.0`)
httpbin-up:
    #!/usr/bin/env bash
    set -euo pipefail
    tag="${HTTPBIN_TAG:-mccutchen/go-httpbin:v2.15.0}"
    container="${COMPOSE_PROJECT_NAME:-mekhan}-httpbin"
    if docker ps --format '{{{{.Names}}}}' | grep -Fxq "$container"; then
      echo "  ✓ httpbin already running (container: $container)"
      echo "    echo   http://localhost:{{httpbin_port}}/anything/<path>"
      exit 0
    fi
    if docker ps -a --format '{{{{.Names}}}}' | grep -Fxq "$container"; then
      docker start "$container" >/dev/null
    else
      echo "▶ starting httpbin (image: $tag) — echo :{{httpbin_port}}"
      docker run -d --rm --name "$container" \
        -p {{httpbin_port}}:8080 \
        "$tag" >/dev/null
    fi
    echo "  ✓ httpbin up"
    echo "    echo   http://localhost:{{httpbin_port}}/anything/<path>  (used by the 11-http-call demo)"

httpbin-down:
    #!/usr/bin/env bash
    set -euo pipefail
    container="${COMPOSE_PROJECT_NAME:-mekhan}-httpbin"
    if docker ps --format '{{{{.Names}}}}' | grep -Fxq "$container"; then
      docker stop "$container" >/dev/null
      echo "  ✓ httpbin stopped"
    else
      echo "  · httpbin not running"
    fi

# ── Postgres demo fixture (19-postgres-node) ─────────────────────────────────
#
# The `19-postgres-node` demo reads/writes a DEDICATED `demo_pg.widgets` fixture
# table (NEVER mekhan's own application tables). This applies the demo's
# idempotent seed against the dev Postgres so the demo + its `mekhan test`
# fixtures have a stable baseline (schema `demo_pg`, three rows: alpha/bravo/
# charlie). Re-run any time to reset to baseline.
#
# Uses the same {{database_url}} the rest of dev points at (slot-aware via env).
# The `demo_pg` resource the demo binds hardcodes the slot-0 port (15439); on a
# slotted worktree edit demos/resources/demo_pg.json to match before going live.
pg-demo-seed:
    #!/usr/bin/env bash
    set -euo pipefail
    cd ..
    echo "▶ seeding demo_pg.widgets fixture into {{database_url}}"
    psql "{{database_url}}" -v ON_ERROR_STOP=1 -f demos/19-postgres-node/seed.sql
    echo "  ✓ demo_pg.widgets seeded (schema demo_pg, 3 baseline rows)"

# ── Executor (aithericon-executor-service, daemon mode) ──────────────────────

up-executor:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs
    if [ -s "{{pid_dir}}/executor.pid" ] && kill -0 "$(cat {{pid_dir}}/executor.pid)" 2>/dev/null; then
      echo "  executor already running (pid $(cat {{pid_dir}}/executor.pid))"
      exit 0
    fi
    echo "▶ building executor (features: {{executor_features}})…"
    (cd executor && cargo build -p aithericon-executor-service --features "{{executor_features}}")

    mkdir -p "{{work_dir}}/executor"
    # Write a fresh executor.toml each `up` so config drift can't accumulate.
    # base_dir is absolute so the run dir + runner script paths don't compound
    # with the executor's CWD when launching the subprocess.
    exec_base="$PWD/{{work_dir}}/executor"
    cat > "{{work_dir}}/executor/executor.toml" <<EOF
    name        = "dev-executor"
    nats_url    = "{{nats_url}}"
    namespace   = "executor"
    base_dir    = "$exec_base"
    concurrency = 4

    [storage]
    backend  = "s3"
    endpoint = "{{s3_endpoint}}"
    # Must match mekhan-service's MEKHAN__S3__BUCKET so the executor can pull
    # node files that mekhan uploaded at publish time. See mekhan.toml.
    bucket   = "mekhan-artifacts"
    region   = "us-east-1"

    [storage.credentials]
    access_key = "rustfsadmin"
    secret_key = "rustfsadmin"

    [logs]
    enabled = true
    sinks   = [{ type = "file" }, { type = "nats", min_level = "info" }]

    [metrics]
    enabled = true
    sinks   = [{ type = "memory" }, { type = "nats" }]

    [python]
    enabled    = true
    prefer_uv  = true

    [cancel]
    nats      = true
    http      = true
    http_port = {{cancel_port}}
    EOF

    echo "▶ starting executor…"
    # CWD must contain executor.toml — executor loads `./executor.toml`.
    # Use bash -c so we can `cd` before exec inside nohup's child shell.
    # VAULT_ADDR is set so PlanSecretsHook can unwrap per-job wrapping tokens
    # the engine attaches; VAULT_TOKEN is intentionally NOT set on the
    # executor — it authenticates by exchanging the wrapping token itself.
    # Honor CARGO_TARGET_DIR so the binary lookup follows the same
    # redirect cargo did. Shared-target setups (umbrella .envrc) put
    # everything under `~/.cache/cargo-targets/...`; the bare workspace
    # fallback is what cargo uses when no env var is set.
    bin="${CARGO_TARGET_DIR:-$PWD/executor/target}/debug/aithericon-executor-service"
    # Robust launch: fully detached + early-crash detection (no phantom ✓ /
    # empty-log mystery). spawn-daemon.sh writes the pidfile, prints the live
    # PID, and `exit 1`s with the log tail if the binary dies within 1s —
    # which (set -euo pipefail active) aborts `up` instead of lying.
    pid=$(just/scripts/spawn-daemon.sh executor "{{pid_dir}}/executor.pid" "{{log_dir}}/executor.log" \
      -- env RUST_LOG="info,aithericon_executor=debug" \
        VAULT_ADDR="{{vault_addr}}" \
        bash -c 'cd "{{work_dir}}/executor" && exec "$0"' "$bin")
    echo "  ✓ executor (pid $pid)  log: {{log_dir}}/executor.log"

down-executor:
    @just dev::_kill executor

# ── Engine (engine/core-engine) ──────────────────────────────────────────────

# scheduler="nomad" additionally wires the engine to a Nomad scheduler
# backend so `Scheduled` AutomatedSteps (which use the datacenter lease
# adapter pattern) can actually dispatch. Default "off" keeps dev minimal
# (the plain `up` path is unchanged). Use `just dev scheduler-up` rather
# than calling this directly with nomad — it also stands up the agent
# and registers the job templates.
up-engine scheduler="off":
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs
    if [ -s "{{pid_dir}}/engine.pid" ] && kill -0 "$(cat {{pid_dir}}/engine.pid)" 2>/dev/null; then
      echo "  engine already running (pid $(cat {{pid_dir}}/engine.pid))"
      exit 0
    fi
    echo "▶ building engine…"
    (cd engine && cargo build -p core-engine)

    # core-engine's default features already include `nomad` + `executor`
    # (engine/core-engine/Cargo.toml), so no rebuild is needed — the engine
    # only needs the scheduler env to initialise the cluster client that the
    # lease adapter's resource_lease_acquire effect uses via ClusterRegistry.
    # Exported (not -i) so the `env` invocation below inherits them.
    if [ "{{scheduler}}" = "nomad" ]; then
      echo "  ↳ scheduler: Nomad (datacenter lease infrastructure)"
      export SCHEDULER_BACKEND="nomad"
      export NOMAD_ADDR="http://127.0.0.1:4646"
      export SCHEDULER_JOB_TEMPLATE="petri-executor-worker"
      export SCHEDULER_SIGNAL_ROUTES="running:sig_running,completed:sig_completed,failed:sig_failed"
      # Self-heal the parameterized jobs at every engine (re)start: the in-memory
      # dev nomad agent loses them on restart, so the next resource_lease_acquire
      # 500s with 'parameterized job not found'. scheduler-up renders the
      # env-interpolated job bodies into {{nomad_jobs_dir}}; the engine reads + POSTs
      # each on startup when NOMAD_AUTOPROVISION_JOBS=1 (never set in prod).
      export NOMAD_AUTOPROVISION_JOBS="1"
      export NOMAD_JOB_TEMPLATE_DIR="$PWD/{{nomad_jobs_dir}}"
    fi
    if [ "{{scheduler}}" = "slurm" ]; then
      # Slurm reaches the cluster over SSH against the local docker-compose
      # cluster (engine/infra/slurm/), polls squeue/sacct, and publishes
      # status signals via NATS. Reverse the whole layer with
      # `just dev slurm-down`.
      echo "  ↳ scheduler: Slurm (datacenter lease infrastructure)"
      export SCHEDULER_BACKEND="slurm"
      export SCHEDULER_JOB_TEMPLATE="mekhan-executor-worker"
      export SCHEDULER_SIGNAL_ROUTES="running:sig_running,completed:sig_completed,failed:sig_failed,timed_out:sig_failed"
      export SLURM_SSH_HOST="localhost"
      export SLURM_SSH_PORT="2222"
      export SLURM_SSH_USER="testuser"
      export SLURM_SSH_KEY="$PWD/engine/infra/slurm/ssh/slurm_test"
      export SLURM_SSH_KNOWN_HOSTS="accept"
      export SLURM_POLL_INTERVAL_SECS="2"
      export SLURM_TEMPLATE_DIR="/opt/petri/templates"
    fi

    echo "▶ starting engine on :{{engine_port}}…"
    # VAULT_ADDR + VAULT_TOKEN flip the engine's `executor-vault-secrets`
    # path on: at job-submit time it scans the spec for secret-template
    # refs, resolves them via VaultSecretStore, and wraps them into a
    # single-use wrapping token attached to the job. Without these env
    # vars, secrets travel unresolved and the executor's PlanSecretsHook
    # falls back to EnvVarSecretStore (which can't resolve resource paths).
    pid=$(just/scripts/spawn-daemon.sh engine "{{pid_dir}}/engine.pid" "{{log_dir}}/engine.log" \
      -- env \
        RUST_LOG="info,petri_application=debug,petri_api=debug" \
        NATS_URL="{{nats_url}}" \
        EXECUTOR_NATS_URL="{{nats_url}}" \
        EXECUTOR_ENABLED="true" \
        EXECUTOR_NAMESPACE="executor" \
        VAULT_ADDR="{{vault_addr}}" \
        VAULT_TOKEN="{{vault_token}}" \
        PORT="{{engine_port}}" \
        "${CARGO_TARGET_DIR:-$PWD/engine/target}/debug/core-engine")
    echo "  ✓ engine (pid $pid)  {{engine_url}}  log: {{log_dir}}/engine.log"

down-engine:
    @just dev::_kill engine

# ── Nomad scheduler layer (for `Scheduled` AutomatedSteps) ───────────────────
#
# Stands up the real-Nomad dispatch path on top of a running dev stack so a
# `Scheduled` AutomatedStep can actually run. The compiled parent net uses the
# datacenter lease adapter pattern — mekhan-service deploys a per-resource
# `pool-{resource_id}` adapter net that acquires a lease via the engine's
# `resource_lease_acquire` effect, then enqueues work to the held allocation's
# drain executor:
#
#   parent-net ─▶ pool-{resource_id} (lease adapter) ─(resource_lease_acquire)─▶
#                 petri-lease-executor (drain) ─▶ executor ─▶ result ─▶ parent
#
# Requires `just dev up` already running (reuses its NATS/rustfs + executor
# binary) and the `nomad` binary on PATH. Restarts the engine with the
# scheduler env. Reverse with `just dev scheduler-down`.
scheduler-up:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs

    # 1. Nomad dev agent (pidfile-managed so `down`/`status` track it).
    if curl -sf --max-time 2 http://127.0.0.1:4646/v1/status/leader >/dev/null 2>&1; then
      echo "▶ nomad: already running"
    else
      command -v nomad >/dev/null 2>&1 || {
        echo "✗ nomad binary not on PATH — brew install hashicorp/tap/nomad" >&2; exit 1; }
      echo "▶ starting nomad dev agent…"
      nohup nomad agent -dev -log-level WARN -config engine/infra/nomad/dev.hcl \
        > "{{log_dir}}/nomad.log" 2>&1 &
      echo $! > "{{pid_dir}}/nomad.pid"
      for _ in $(seq 1 30); do
        curl -sf --max-time 2 http://127.0.0.1:4646/v1/status/leader >/dev/null 2>&1 && break
        sleep 1
      done
      curl -sf --max-time 2 http://127.0.0.1:4646/v1/status/leader >/dev/null 2>&1 || {
        echo "✗ nomad did not become ready (log: {{log_dir}}/nomad.log)" >&2; exit 1; }
      echo "  ✓ nomad (pid $(cat {{pid_dir}}/nomad.pid))  http://localhost:4646"
    fi

    # 2. Executor binary — reused by Nomad per job (run_to_completion).
    exec_bin="${CARGO_TARGET_DIR:-$PWD/executor/target}/debug/aithericon-executor-service"
    if [ ! -x "$exec_bin" ]; then
      echo "▶ building executor (features: {{executor_features}})…"
      (cd executor && cargo build -p aithericon-executor-service --features "{{executor_features}}")
    fi

    # 3. Register the petri-executor-worker parameterized job: inject the
    #    binary path + the dev S3 (rustfs) storage env so the Nomad-spawned
    #    executor can pull node files mekhan staged at publish time. Bucket
    #    must match mekhan-service's MEKHAN__S3__BUCKET (see up-executor).
    #    `EXECUTOR_NAMESPACE` MUST match the engine's namespace (`executor`
    #    in dev — see up-engine) so the Nomad-spawned worker consumes the
    #    queue executor-net's `executor_submit` publishes to. The upstream
    #    template ships `executor_jobs` (a GP-era default) which silently
    #    diverts work to the always-on dev daemon and leaves the Nomad
    #    worker idle — proven false-Nomad-execution; explicitly override.
    echo "▶ registering petri-executor-worker Nomad job template…"
    # Render the interpolated job body to a stable dir AND register it now. The
    # rendered file is the single source of truth the engine re-POSTs on every
    # (re)start (NOMAD_AUTOPROVISION_JOBS, see up-engine) so a restarted/cleared
    # in-memory nomad agent self-heals; the curl here is an idempotent belt-and-
    # suspenders for the immediate scheduler-up.
    mkdir -p "{{nomad_jobs_dir}}"
    jq --arg bin "$exec_bin" --arg ep "{{s3_endpoint}}" \
       '.Job.TaskGroups[0].Tasks[0].Config.command = $bin
        | .Job.TaskGroups[0].Tasks[0].Env += {
            "EXECUTOR_NAMESPACE": "executor",
            "EXECUTOR_STORAGE__BACKEND": "s3",
            "EXECUTOR_STORAGE__ENDPOINT": $ep,
            "EXECUTOR_STORAGE__BUCKET": "mekhan-artifacts",
            "EXECUTOR_STORAGE__REGION": "us-east-1",
            "EXECUTOR_STORAGE__CREDENTIALS__ACCESS_KEY": "rustfsadmin",
            "EXECUTOR_STORAGE__CREDENTIALS__SECRET_KEY": "rustfsadmin",
            "EXECUTOR_PYTHON__ENABLED": "true",
            "EXECUTOR_PYTHON__PREFER_UV": "true"
          }' engine/infra/nomad/executor-job-template.json > "{{nomad_jobs_dir}}/executor-job.json"
    curl -sf -X POST http://127.0.0.1:4646/v1/jobs -d @"{{nomad_jobs_dir}}/executor-job.json" >/dev/null
    echo "  ✓ petri-executor-worker registered (binary: $exec_bin)"

    # 3b. Register the petri-lease-executor parameterized job (the LEASE path —
    #     the Nomad analogue of mekhan-lease-executor.sh). Unlike the worker
    #     above (PerJob, one-shot via EXECUTOR_TARGET_EXEC_ID), the dispatched
    #     child is ONE persistent Pool/drain executor: NO TARGET_EXEC_ID, so it
    #     opens a durable consumer on the lease-scoped namespace (lease-<grant>,
    #     from dispatch Meta) and drains EVERY job the leased loop enqueues there
    #     — keeping warm state across iterations. NomadAllocatorClient::acquire
    #     dispatches this; release = `nomad job stop`. Serial drain hardening:
    #     EXECUTOR_MAX_ACK_PENDING=1 + a stable shared venv cache so each lease's
    #     first job is warm (a cold venv build blows the ack window → redelivery).
    echo "▶ registering petri-lease-executor Nomad job template…"
    jq --arg bin "$exec_bin" --arg ep "{{s3_endpoint}}" \
       --arg nats "{{nats_url}}" --arg sdk "$PWD/executor/packages/aithericon-sdk" \
       '.Job.TaskGroups[0].Tasks[0].Config.command = $bin
        | .Job.TaskGroups[0].Tasks[0].Env += {
            "EXECUTOR_NATS_URL": $nats,
            "EXECUTOR_STORAGE__BACKEND": "s3",
            "EXECUTOR_STORAGE__ENDPOINT": $ep,
            "EXECUTOR_STORAGE__BUCKET": "mekhan-artifacts",
            "EXECUTOR_STORAGE__REGION": "us-east-1",
            "EXECUTOR_STORAGE__CREDENTIALS__ACCESS_KEY": "rustfsadmin",
            "EXECUTOR_STORAGE__CREDENTIALS__SECRET_KEY": "rustfsadmin",
            "AITHERICON_SDK_PATH": $sdk,
            "EXECUTOR_PYTHON__ENABLED": "true",
            "EXECUTOR_PYTHON__PREFER_UV": "true",
            "EXECUTOR_PYTHON__CACHE_DIR": "/tmp/aithericon-nomad-lease-venvs",
            "EXECUTOR_MAX_ACK_PENDING": "1"
          }' engine/infra/nomad/lease-executor-job-template.json > "{{nomad_jobs_dir}}/lease-executor-job.json"
    curl -sf -X POST http://127.0.0.1:4646/v1/jobs -d @"{{nomad_jobs_dir}}/lease-executor-job.json" >/dev/null
    echo "  ✓ petri-lease-executor registered (drain/Pool parameterized job)"

    # 4. Pause the dev daemon — both it and the Nomad-spawned worker would
    #    consume the same `executor` namespace; the warm daemon races and
    #    wins, leaving the Nomad worker provably inert. Stopping it makes
    #    the Nomad-dispatched worker the sole consumer, so a Scheduled run
    #    genuinely exercises the Nomad path end-to-end (staging + python +
    #    result). Restored by `scheduler-down`. Consequence: while the
    #    scheduler layer is up, INLINE AutomatedSteps cannot run.
    echo "▶ pausing dev executor daemon (Nomad worker takes over `executor` queue)…"
    just dev::down-executor
    # Defensively kill any UNTRACKED aithericon-executor-service processes —
    # `down-executor` only touches the main-repo pidfile, so a stray daemon
    # left over from a worktree session (see [[feedback_worktree_agent_cwd]])
    # would still consume jobs off `executor` and silently shadow the Nomad
    # worker. SIGTERM first + brief wait so apalis can drop its JetStream
    # consumers cleanly — SIGKILL leaves them registered, and the broader
    # filter (`executor.<priority>.>`) blocks the next per-job executor from
    # claiming its specific `executor.<priority>.<exec_id>` subject ("filtered
    # consumer not unique on workqueue stream"). Final SIGKILL only if a stray
    # process refused to terminate. Nomad/Slurm-launched executors are short-
    # lived per dispatch and respawned per resource_lease_acquire, so killing them
    # is harmless.
    pkill -TERM -f aithericon-executor-service 2>/dev/null || true
    for _ in $(seq 1 10); do
      pgrep -f aithericon-executor-service >/dev/null 2>&1 || break
      sleep 0.5
    done
    pkill -KILL -f aithericon-executor-service 2>/dev/null || true

    # Apalis-NATS consumer purge. The dev daemon's executor binary creates
    # catch-all (`filter_subject=None`) consumers on the `executor_{high,
    # medium,low}` workqueue streams; apalis does NOT delete them on
    # SIGTERM, and JetStream's workqueue retention rejects a new consumer
    # whose filter overlaps with an existing one — so the next per-job
    # executor's filtered consumer (`executor.<priority>.<exec_id>`) gets
    # `filtered consumer not unique on workqueue stream (code 400)` and
    # exits without pulling its job. Drop the catch-alls; apalis recreates
    # what it needs on next executor boot. Idempotent (404 = already gone).
    if command -v nats >/dev/null 2>&1; then
      for stream in executor_high executor_medium executor_low; do
        nats --server "{{nats_url}}" consumer rm "$stream" "${stream}_consumer" --force 2>/dev/null || true
      done
    fi

    # 5. Restart the engine WITH the scheduler env so the ClusterRegistry
    #    initialises the Nomad client that the lease adapter's
    #    resource_lease_acquire effect uses.
    just dev::down-engine
    just dev::up-engine nomad
    echo "▶ waiting for engine API…"
    for _ in $(seq 1 30); do
      curl -sf {{engine_url}}/api/nets/metadata >/dev/null 2>&1 && break
      sleep 1
    done
    curl -sf {{engine_url}}/api/nets/metadata >/dev/null 2>&1 || {
      echo "✗ engine did not come back (log: {{log_dir}}/engine.log)" >&2; exit 1; }
    sleep 2  # let the Nomad + executor watchers connect

    echo ""
    echo "✓ Nomad scheduler layer up:"
    echo "  nomad        → http://localhost:4646/ui"
    echo "  engine running with SCHEDULER_BACKEND=nomad on :{{engine_port}}"
    echo "  lease adapter nets deployed per-resource by mekhan-service"
    echo "  dev daemon executor PAUSED — Nomad worker is the sole consumer"
    echo "    of the 'executor' queue. Inline AutomatedSteps will NOT run"
    echo "    until you revert."
    echo ""
    echo "  revert : just dev scheduler-down"

# Tear down the Nomad scheduler layer; engine returns to plain dev config.
scheduler-down:
    #!/usr/bin/env bash
    set -euo pipefail
    if curl -sf --max-time 2 http://127.0.0.1:4646/v1/jobs >/dev/null 2>&1; then
      echo "▶ purging Nomad child jobs…"
      for jid in $(curl -sf http://127.0.0.1:4646/v1/jobs 2>/dev/null \
        | jq -r '.[] | select(.ParentID != "") | .ID' 2>/dev/null); do
        curl -sf -X DELETE "http://127.0.0.1:4646/v1/job/$jid?purge=true" >/dev/null 2>&1 || true
      done
    fi
    just dev::_kill nomad || true
    pkill -f "nomad agent -dev" 2>/dev/null || true
    echo "▶ restarting engine without scheduler env…"
    just dev::down-engine
    just dev::up-engine
    echo "▶ resuming dev executor daemon…"
    just dev::up-executor
    echo "  ✓ scheduler layer down; engine + dev daemon back on plain dev config"

# ── Resource-pool layer (shared capacity pool — the docs/14 reference pattern) ─
#
# Deploys the long-lived `resource-pool-net` (engine/sdk/examples/resource_pool_net.rs)
# onto the ALREADY-RUNNING dev engine, seeded with N=2 GPU capacity tokens.
# Unlike `scheduler-up` this needs NO engine restart — it is just another net
# on :13030. The instance nets that contend for it are produced by the M3
# compiler (an AutomatedStep with `resourcePool`), so mekhan must be built WITH
# the resource-pool branch. Requires `just dev up` already running.
#
# Then `just dev resource-pool-demo` publishes the 13-resource-pool demo and
# fires K instances; watch the contention drain at /nets/resource-pool.
resource-pool-up:
    #!/usr/bin/env bash
    set -euo pipefail
    curl -sf --max-time 2 http://localhost:13030/api/nets/metadata >/dev/null 2>&1 || {
      echo "✗ engine not reachable on :13030 — run 'just dev' first" >&2; exit 1; }
    echo "▶ building resource_pool_net example + aithericon CLI…"
    (cd engine && cargo build -p aithericon-sdk --example resource_pool_net)
    cd engine
    [ -x ./target/debug/aithericon ] || cargo build --bin aithericon
    echo "▶ deploying + activating resource-pool-net (seeded with 2 GPU units)…"
    # The dev engine is on :13030 (the SDK example + aithericon CLI default to
    # :3030), so point both at it explicitly.
    ./target/debug/examples/resource_pool_net --deploy --net-id resource-pool-net --url http://localhost:13030 >/dev/null
    ./target/debug/aithericon activate resource-pool-net --url http://localhost:13030
    echo ""
    echo "✓ resource-pool-net deployed & Running on :13030 (pool = 2 capacity units)"
    echo "  dashboard → http://localhost:15173/nets/resource-pool"
    echo "  drive it  → just dev resource-pool-demo [n=4]"
    echo "  reap a crashed holder (money shot):"
    echo "    engine/target/debug/aithericon inject resource-pool-net lease_expired '{\"grant_id\":\"<instanceId>:render\"}' --url http://localhost:13030"

# Publish the 13-resource-pool demo and launch N concurrent instances against
# the pool. Run `resource-pool-up` FIRST — the instance nets bridge to
# resource-pool-net, which must be deployed before they can activate (Strict
# bridge gate). Default N=4 vs the pool's N=2 → 2 render, 2 queue.
resource-pool-demo n="4":
    #!/usr/bin/env bash
    set -euo pipefail
    TID="00000000-0000-0000-0000-000000000060"
    BASE="http://localhost:13100"
    curl -sf --max-time 2 "$BASE/healthz" >/dev/null 2>&1 || {
      echo "✗ mekhan not reachable on :13100 — run 'just dev' first" >&2; exit 1; }
    echo "▶ ensuring 13-resource-pool is published (template $TID)…"
    code=$(curl -s -o /dev/null -w "%{http_code}" -X POST "$BASE/api/v1/templates/$TID/publish" -H 'content-type: application/json')
    case "$code" in
      200|201) echo "  published";;
      409)     echo "  already published (ok)";;
      404)     echo "✗ template not found — seed it first: 'just dev demos-reseed'" >&2; exit 1;;
      *)       echo "✗ publish failed (HTTP $code)" >&2; exit 1;;
    esac
    echo "▶ launching {{n}} render instances (Start field job_name=shot-N)…"
    for i in $(seq 1 {{n}}); do
      curl -sf -X POST "$BASE/api/v1/instances" -H 'content-type: application/json' \
        -d "{\"template_id\":\"$TID\",\"start_tokens\":[{\"start_block_id\":\"start\",\"token\":{\"job_name\":\"shot-$i\"}}],\"metadata\":{\"demo\":\"resource-pool\"}}" \
        >/dev/null && echo "  ✓ launched shot-$i" || echo "  ✗ shot-$i failed"
    done
    echo ""
    echo "✓ {{n}} instances launched. With N=2 capacity, 2 render now and the rest"
    echo "  queue in the pool's claim_inbox until a GPU frees (admission control)."
    echo "  watch → http://localhost:15173/nets/resource-pool"

# ── Slurm scheduler layer (for `Scheduled` AutomatedSteps via Slurm) ─────────
#
# Mirror of `scheduler-up` but using the local docker-compose Slurm cluster
# (engine/infra/slurm/) instead of Nomad. The lease adapter pattern is
# identical — only the backend that `resource_lease_acquire` dispatches to
# changes (sbatch over SSH instead of Nomad dispatch):
#
#   parent-net ─▶ pool-{resource_id} (lease adapter) ─(resource_lease_acquire)─▶
#                 mekhan-lease-executor (sbatch) ─▶ executor ─▶ result ─▶ parent
#
# Requires `just dev up` already running (reuses NATS/rustfs), docker +
# cargo-zigbuild on PATH (the executor binary must be Linux x86_64 since
# Slurm runs inside Docker). Reverse with `just dev slurm-down`.
slurm-up:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs

    # 1. Slurm cluster (docker-compose: slurmctld + slurmd + sshd in one
    #    container). Reuse the engine subrepo's existing recipes — they
    #    own the image/Dockerfile and the SSH key. Idempotent: `slurm-up`
    #    is a `docker compose up -d`.
    echo "▶ starting local Slurm cluster…"
    (cd engine && just infra slurm-up)
    (cd engine && just infra slurm-wait)

    # 2. Cross-compile the executor for Linux x86_64. The Slurm container is
    #    `linux/amd64`, so the host-native macOS binary is unusable — we ship
    #    a freshly-zigbuilt one. `cargo-zigbuild` is the same toolchain the
    #    engine demo recipes use (engine/just/demo.just::slurm-three-layer).
    command -v cargo-zigbuild >/dev/null 2>&1 || {
      echo "✗ cargo-zigbuild not on PATH — cargo install cargo-zigbuild" >&2; exit 1; }
    # Trimmed feature set — the host-side daemon uses {{executor_features}}
    # which adds `docker` and `file-ops`. `docker` is meaningless inside the
    # Slurm container, and `file-ops` pulls libnetcdf+libhdf5 which would
    # need cross-compile system libs we don't have (and aren't exercised by
    # the python-backed e2e). Python + opendal-s3 is what the Scheduled run
    # actually needs (run main.py + pull staged files from rustfs).
    echo "▶ cross-compiling executor for x86_64-unknown-linux-gnu (slim features)…"
    (cd executor && cargo zigbuild -p aithericon-executor-service \
       --features "python,http,http-cancel,opendal-s3,url-inputs" \
       --target x86_64-unknown-linux-gnu)
    exec_bin="$PWD/executor/target/x86_64-unknown-linux-gnu/debug/aithericon-executor-service"
    if [ ! -f "$exec_bin" ]; then
      echo "✗ Linux executor binary not found at $exec_bin" >&2; exit 1; fi

    # 3. Install the binary, the aithericon Python SDK, and the Mekhan-
    #    specific job template (`mekhan-executor-worker.sh`). The SDK
    #    install matters whenever a Python AutomatedStep uses `sdk: true`
    #    (the default): the runner imports `aithericon` from
    #    `AITHERICON_SDK_PATH`; without it, the user script's
    #    `from _aithericon_io import …` fails at import time and the
    #    execution exits ~110ms with status=failed. We don't overwrite
    #    the upstream `executor.sh` — that one is owned by the engine
    #    demos and uses `EXECUTOR_NAMESPACE=executor_jobs` (a GP-era
    #    default that would silently divert work to the always-on dev
    #    daemon and leave the Slurm worker idle — the same false-
    #    execution trap that bit the Nomad recipe). The new template:
    #     - sets EXECUTOR_NAMESPACE=executor (matches up-engine)
    #     - sets AITHERICON_SDK_PATH=/opt/petri/aithericon-sdk
    #     - reaches rustfs S3 + NATS via host.docker.internal (Docker Desktop
    #       maps to the host gateway; see slurm docker-compose extra_hosts)
    #     - bucket mekhan-artifacts (matches mekhan-service's MEKHAN__S3__BUCKET)
    echo "▶ installing executor + SDK + mekhan-executor-worker template into Slurm container…"
    container="$(cd engine && docker compose -f infra/slurm/docker-compose.yml ps -q slurm)"
    docker exec "$container" mkdir -p /opt/petri/bin /opt/petri/templates
    docker cp "$exec_bin" "$container:/opt/petri/bin/executor"
    docker exec "$container" chmod +x /opt/petri/bin/executor
    docker exec "$container" rm -rf /opt/petri/aithericon-sdk
    docker cp executor/packages/aithericon-sdk "$container:/opt/petri/aithericon-sdk"
    docker exec "$container" chmod -R a+rwX /opt/petri/aithericon-sdk

    tpl="$(mktemp)"
    cat > "$tpl" <<'TEMPLATE'
    #!/bin/bash
    #SBATCH --output=/tmp/petri-executor-%j.out
    export EXECUTOR_NATS_URL=nats://host.docker.internal:{{nats_port}}
    export EXECUTOR_NAMESPACE=executor
    export EXECUTOR_SOURCE=nats_queue
    export EXECUTOR_LIFETIME=run_to_completion
    export EXECUTOR_MAX_JOBS=1
    export EXECUTOR_CONCURRENCY=1
    export EXECUTOR_NAME="slurm-executor-${SLURM_JOB_ID}"
    export EXECUTOR_BASE_DIR=/tmp/aithericon-executor-slurm/${SLURM_JOB_ID}
    export EXECUTOR_DEFAULT_TIMEOUT_SECS=60
    export EXECUTOR_IDLE_TIMEOUT_SECS=15
    export AITHERICON_SDK_PATH=/opt/petri/aithericon-sdk
    export EXECUTOR_STORAGE__BACKEND=s3
    export EXECUTOR_STORAGE__ENDPOINT=http://host.docker.internal:{{s3_port}}
    export EXECUTOR_STORAGE__BUCKET=mekhan-artifacts
    export EXECUTOR_STORAGE__REGION=us-east-1
    export EXECUTOR_STORAGE__CREDENTIALS__ACCESS_KEY=rustfsadmin
    export EXECUTOR_STORAGE__CREDENTIALS__SECRET_KEY=rustfsadmin
    export EXECUTOR_PYTHON__ENABLED=true
    export EXECUTOR_PYTHON__PREFER_UV=true
    echo "PETRI_TOKEN_DATA=$PETRI_TOKEN_DATA"
    echo "Starting executor (SLURM job ${SLURM_JOB_ID})"
    /opt/petri/bin/executor
    TEMPLATE
    docker cp "$tpl" "$container:/opt/petri/templates/mekhan-executor-worker.sh"
    # Explicit 0755 — `mktemp` creates 0600, and a follow-up `chmod +x` would
    # add only the execute bits → 0711, leaving the file unreadable by other
    # users and triggering `sbatch: error: Unable to open file` from inside
    # the Slurm container.
    docker exec "$container" chmod 0755 /opt/petri/templates/mekhan-executor-worker.sh
    rm -f "$tpl"
    echo "  ✓ mekhan-executor-worker.sh installed (binary: /opt/petri/bin/executor)"

    # 3b. Install the PERSISTENT DRAIN executor template (the lease path),
    #     ALONGSIDE the PerJob worker above (which stays for the `submit` path).
    #     `slurm_allocator.rs::acquire_lease` srun's this onto the held alloc on
    #     lease acquire; it runs ONE long-lived executor in Pool/drain mode that
    #     consumes the lease-scoped namespace and pulls EVERY job the leased loop
    #     enqueues there — keeping warm state (venv/model/GPU) across iterations.
    #     The single load-bearing diff from the worker template: NO
    #     EXECUTOR_TARGET_EXEC_ID (its ABSENCE selects Pool mode — a durable
    #     shared consumer on `{ns}_{prio}` with a wildcard `{ns}.{prio}.>`
    #     filter). NAMESPACE/MAX_JOBS/IDLE_TIMEOUT come from the `srun --export`
    #     the acquire path stamps (LEASE_NAMESPACE=lease-<grant_id> etc); the
    #     long idle timeout lets the executor survive inter-iteration gaps but
    #     self-exit if the loop wedges. scancel on release SIGTERMs it → graceful
    #     drain → exit, freeing the nodes.
    tpl2="$(mktemp)"
    cat > "$tpl2" <<'TEMPLATE'
    #!/bin/bash
    #SBATCH --output=/tmp/petri-lease-executor-%j.out
    export EXECUTOR_NATS_URL=nats://host.docker.internal:{{nats_port}}
    export EXECUTOR_NAMESPACE="${LEASE_NAMESPACE}"
    export EXECUTOR_SOURCE=nats_queue
    export EXECUTOR_LIFETIME=run_to_completion
    export EXECUTOR_MAX_JOBS="${LEASE_MAX_JOBS}"
    export EXECUTOR_CONCURRENCY=1
    # Serial drain: deliver at most one un-acked message at a time. Without this
    # the durable Pool consumer hands the whole queue to a concurrency=1 worker;
    # messages queued behind a slow job (cold venv build) exceed their ack window
    # and get redelivered → the same iteration re-runs. Bound to the worker's
    # parallelism (1) so JetStream never over-delivers.
    export EXECUTOR_MAX_ACK_PENDING=1
    export EXECUTOR_NAME="lease-executor-${SLURM_JOB_ID}"
    export EXECUTOR_BASE_DIR=/tmp/aithericon-executor-lease/${SLURM_JOB_ID}
    # Share the venv cache ACROSS lease executors (stable, not under the
    # per-job base_dir). Otherwise every new lease cold-builds its first venv
    # (~8s) — which, on a 275ms job, blows the message's ack window and the
    # consumer redelivers the same iteration. A stable cache makes the first job
    # of each lease warm too, so each iteration runs exactly once.
    export EXECUTOR_PYTHON__CACHE_DIR=/tmp/aithericon-lease-venvs
    export EXECUTOR_DEFAULT_TIMEOUT_SECS=60
    export EXECUTOR_IDLE_TIMEOUT_SECS="${LEASE_IDLE_TIMEOUT:-300}"
    export AITHERICON_SDK_PATH=/opt/petri/aithericon-sdk
    export EXECUTOR_STORAGE__BACKEND=s3
    export EXECUTOR_STORAGE__ENDPOINT=http://host.docker.internal:{{s3_port}}
    export EXECUTOR_STORAGE__BUCKET=mekhan-artifacts
    export EXECUTOR_STORAGE__REGION=us-east-1
    export EXECUTOR_STORAGE__CREDENTIALS__ACCESS_KEY=rustfsadmin
    export EXECUTOR_STORAGE__CREDENTIALS__SECRET_KEY=rustfsadmin
    export EXECUTOR_PYTHON__ENABLED=true
    export EXECUTOR_PYTHON__PREFER_UV=true
    echo "Starting lease drain executor (SLURM job ${SLURM_JOB_ID}) ns=${LEASE_NAMESPACE} max_jobs=${LEASE_MAX_JOBS} idle=${LEASE_IDLE_TIMEOUT:-300}"
    /opt/petri/bin/executor
    TEMPLATE
    docker cp "$tpl2" "$container:/opt/petri/templates/mekhan-lease-executor.sh"
    docker exec "$container" chmod 0755 /opt/petri/templates/mekhan-lease-executor.sh
    rm -f "$tpl2"
    echo "  ✓ mekhan-lease-executor.sh installed (drain/Pool template for the lease path)"

    # 4. Pause the dev daemon — same reason as scheduler-up (Nomad): both it
    #    and the Slurm-spawned worker would consume the same `executor`
    #    namespace; the warm daemon races and wins, leaving the Slurm worker
    #    provably inert. SIGTERM-first defensive cleanup so apalis can drop
    #    its JetStream consumers — see scheduler-up's identical block for the
    #    rationale (SIGKILL leaves stale `executor.<priority>.>` consumers
    #    that block the next per-job executor's specific filter). While the
    #    layer is up, INLINE AutomatedSteps cannot run.
    echo "▶ pausing dev executor daemon (Slurm worker takes over `executor` queue)…"
    just dev::down-executor
    pkill -TERM -f aithericon-executor-service 2>/dev/null || true
    for _ in $(seq 1 10); do
      pgrep -f aithericon-executor-service >/dev/null 2>&1 || break
      sleep 0.5
    done
    pkill -KILL -f aithericon-executor-service 2>/dev/null || true

    # Apalis-NATS consumer purge — see scheduler-up's identical block for the
    # rationale. Without this the Slurm-launched executor's per-job filtered
    # consumer collides with the dev daemon's catch-all on the workqueue
    # stream and exits idle. Idempotent (404 = already gone).
    if command -v nats >/dev/null 2>&1; then
      for stream in executor_high executor_medium executor_low; do
        nats --server "{{nats_url}}" consumer rm "$stream" "${stream}_consumer" --force 2>/dev/null || true
      done
    fi

    # 5. Restart the engine WITH the Slurm scheduler env so the ClusterRegistry
    #    initialises the Slurm client that the lease adapter's
    #    resource_lease_acquire effect uses.
    just dev::down-engine
    just dev::up-engine slurm
    echo "▶ waiting for engine API…"
    for _ in $(seq 1 30); do
      curl -sf {{engine_url}}/api/nets/metadata >/dev/null 2>&1 && break
      sleep 1
    done
    curl -sf {{engine_url}}/api/nets/metadata >/dev/null 2>&1 || {
      echo "✗ engine did not come back (log: {{log_dir}}/engine.log)" >&2; exit 1; }
    sleep 2  # let the Slurm + executor watchers connect

    echo ""
    echo "✓ Slurm scheduler layer up:"
    echo "  slurm        → docker compose (login: ssh -p 2222 testuser@localhost)"
    echo "  engine running with SCHEDULER_BACKEND=slurm on :{{engine_port}}"
    echo "  lease adapter nets deployed per-resource by mekhan-service"
    echo "  dev daemon executor PAUSED — Slurm worker is the sole consumer"
    echo "    of the 'executor' queue. Inline AutomatedSteps will NOT run"
    echo "    until you revert."
    echo ""
    echo "  revert : just dev slurm-down"

# Tear down the Slurm scheduler layer; engine returns to plain dev config.
slurm-down:
    #!/usr/bin/env bash
    set -euo pipefail
    # Cancel anything pending/running so the dev cluster doesn't carry state
    # across runs (idle-out is configured short on our template, but be safe).
    if (cd engine && docker compose -f infra/slurm/docker-compose.yml ps -q slurm >/dev/null 2>&1); then
      echo "▶ cancelling Slurm jobs (sacct lookback)…"
      ssh -o StrictHostKeyChecking=no -o IdentitiesOnly=yes \
          -i engine/infra/slurm/ssh/slurm_test -p 2222 testuser@localhost \
          'for j in $(squeue -h -u testuser -o %i); do scancel "$j"; done' \
          2>/dev/null || true
      (cd engine && just infra slurm-down) || true
    fi
    echo "▶ restarting engine without scheduler env…"
    just dev::down-engine
    just dev::up-engine
    echo "▶ resuming dev executor daemon…"
    just dev::up-executor
    echo "  ✓ Slurm layer down; engine + dev daemon back on plain dev config"

# ── Mekhan service (Rust backend) ────────────────────────────────────────────

# auth_mode: "dev_noop" (default — offline, every request is the dev user)
# or "bff" (real Zitadel cookie auth + machine-PAT introspection; requires
# `bash deploy/zitadel/bootstrap.sh` to have written mekhan.local.toml).
up-mekhan auth_mode="dev_noop":
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs
    if [ -s "{{pid_dir}}/mekhan.pid" ] && kill -0 "$(cat {{pid_dir}}/mekhan.pid)" 2>/dev/null; then
      echo "  mekhan-service already running (pid $(cat {{pid_dir}}/mekhan.pid))"
      exit 0
    fi
    echo "▶ building mekhan-service…"
    # mekhan-service is a member of the platform-root workspace, so its build
    # output lands in ./target/, not service/target/. Build from repo root.
    cargo build --bin mekhan-service

    # mekhan.toml carries the localhost db / nats / s3 endpoints; env overrides
    # win, so we make sure NATS + DB + S3 match the running infra.
    #
    # dev_noop: MEKHAN__AUTH__MODE=dev_noop overrides mekhan.local.toml's bff
    # mode — NoopAuthenticator makes every request the fixed dev user, no
    # Zitadel needed, fully offline. bff: no override, so mekhan.local.toml's
    # mode=bff + issuer/client_id + introspection creds take effect (real
    # cookie auth for the SPA, RFC 7662 PAT introspection for the CLI).
    # main.rs refuses dev_noop if MEKHAN_ENV=prod.
    mode_assign=""
    if [ "{{auth_mode}}" = "bff" ]; then
      if ! grep -q 'introspection_client_id' service/mekhan.local.toml 2>/dev/null; then
        echo "✗ auth_mode=bff needs introspection creds in service/mekhan.local.toml" >&2
        echo "  run: bash deploy/zitadel/bootstrap.sh" >&2
        exit 1
      fi
      echo "▶ starting mekhan-service on :{{service_port}} (auth: BFF + Zitadel PAT introspection)…"
    else
      mode_assign="MEKHAN__AUTH__MODE=dev_noop"
      echo "▶ starting mekhan-service on :{{service_port}} (auth: dev_noop — offline)…"
    fi
    # VAULT_ADDR + VAULT_TOKEN promote the resource_store from the
    # in-memory placeholder (which loses secrets on every mekhan restart)
    # to a real VaultResourceStore. Resource secrets created via the
    # `/api/v1/resources` CRUD path now land in Vault and survive restarts
    # (until `just dev reset` wipes the dev vault container).
    pid=$(just/scripts/spawn-daemon.sh mekhan "{{pid_dir}}/mekhan.pid" "{{log_dir}}/mekhan.log" \
      -- env \
        RUST_LOG="info,mekhan_service=debug" \
        MEKHAN__DATABASE_URL="{{database_url}}" \
        MEKHAN__NATS_URL="{{nats_url}}" \
        MEKHAN__PORT="{{service_port}}" \
        MEKHAN__PETRI_LAB_URL="{{engine_url}}" \
        MEKHAN__S3__ENDPOINT="{{s3_endpoint}}" \
        MEKHAN__S3__ACCESS_KEY="rustfsadmin" \
        MEKHAN__S3__SECRET_KEY="rustfsadmin" \
        MEKHAN__DEMOS__SEED="true" \
        MEKHAN__DEMOS__DIR="$PWD/demos" \
        VAULT_ADDR="{{vault_addr}}" \
        VAULT_TOKEN="{{vault_token}}" \
        ${CLOUD_LAYER_JWT_SECRET:+CLOUD_LAYER_JWT_SECRET="$CLOUD_LAYER_JWT_SECRET"} \
        ${CLOUD_LAYER_BASE_URL:+CLOUD_LAYER_BASE_URL="$CLOUD_LAYER_BASE_URL"} \
        ${mode_assign:+"$mode_assign"} \
        "${CARGO_TARGET_DIR:-$PWD/target}/debug/mekhan-service")
    echo "  ✓ mekhan-service (pid $pid)  {{service_url}}  log: {{log_dir}}/mekhan.log"

down-mekhan:
    @just dev::_kill mekhan

# ── Mekhan app (SvelteKit dev server) ────────────────────────────────────────

up-app:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs
    if [ -s "{{pid_dir}}/app.pid" ] && kill -0 "$(cat {{pid_dir}}/app.pid)" 2>/dev/null; then
      echo "  app already running (pid $(cat {{pid_dir}}/app.pid))"
      exit 0
    fi
    if [ ! -d app/node_modules ]; then
      echo "▶ installing app deps…"
      (cd app && pnpm install)
    fi
    # BFF model: the SPA has no auth env at all — it only calls
    # GET /api/auth/session, which the dev_noop backend answers with the fixed
    # dev user (no redirect, fully offline). Remove any stale env override a
    # previous (pre-BFF) `just dev` may have written so it can't shadow things.
    rm -f app/.env.development.local
    echo "▶ starting SvelteKit dev server on :{{app_port}}…"
    # vite.config.ts reads MEKHAN_APP_PORT (own port) + MEKHAN_SERVICE_URL
    # (proxy target) from the env; pass them explicitly so this works even
    # when the recipe runs outside a direnv-hooked shell.
    nohup env MEKHAN_APP_PORT="{{app_port}}" MEKHAN_SERVICE_URL="{{service_url}}" \
      bash -c 'cd "$PWD/app" && exec pnpm run dev -- --host 0.0.0.0' \
      > "{{log_dir}}/app.log" 2>&1 &
    echo $! > "{{pid_dir}}/app.pid"
    echo "  ✓ app (pid $(cat {{pid_dir}}/app.pid))  http://localhost:{{app_port}}  log: {{log_dir}}/app.log"

down-app:
    @just dev::_kill app

# ── Internal helpers ─────────────────────────────────────────────────────────

_dirs:
    @mkdir -p "{{pid_dir}}" "{{log_dir}}" "{{work_dir}}"

# Block until the engine has created the PETRI_GLOBAL JetStream stream, so a
# cold `just dev` doesn't race up-mekhan ahead of stream creation (mekhan's
# lifecycle/causality/step-execution consumers attach to PETRI_GLOBAL). This
# is the happy-path belt; the suspenders are the bounded Rust retry in
# MekhanNats::get_stream_with_retry, so a timeout here only WARNS, never fails.
_wait-engine-stream:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "▶ waiting for engine PETRI_GLOBAL stream…"
    for _ in $(seq 1 60); do
      if command -v nats >/dev/null 2>&1; then
        if nats --server "{{nats_url}}" stream info PETRI_GLOBAL >/dev/null 2>&1; then
          echo "  ✓ PETRI_GLOBAL ready"; exit 0
        fi
      elif curl -sf --max-time 2 "{{nats_mon_url}}/jsz?streams=true&accounts=true" 2>/dev/null \
             | grep -q PETRI_GLOBAL; then
        echo "  ✓ PETRI_GLOBAL ready"; exit 0
      fi
      sleep 1
    done
    echo "  ⚠ PETRI_GLOBAL not seen after 60s — mekhan consumers will retry" >&2

# Kill a tracked process group cleanly. SIGTERM first, then SIGKILL after 5s
# if it's still alive (cargo-spawned children sometimes ignore one signal).
_kill name:
    #!/usr/bin/env bash
    set -euo pipefail
    pf="{{pid_dir}}/{{name}}.pid"
    if [ ! -s "$pf" ]; then
      echo "  {{name}} not running (no pidfile)"
      exit 0
    fi
    pid="$(cat "$pf")"
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "  {{name}} not running (stale pidfile)"
      rm -f "$pf"
      exit 0
    fi
    echo "▶ stopping {{name}} (pid $pid)…"
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 10); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.5
    done
    if kill -0 "$pid" 2>/dev/null; then
      echo "  {{name}} did not exit on SIGTERM, sending SIGKILL"
      kill -9 "$pid" 2>/dev/null || true
    fi
    rm -f "$pf"
    echo "  ✓ {{name}} stopped"

_summary auth="DISABLED (dev_noop) — every request is the dev user":
    @echo ""
    @echo "✓ dev stack up:  (slot {{env_var_or_default("WORKTREE_SLOT", "0")}}, project {{env_var_or_default("COMPOSE_PROJECT_NAME", "aithericon-platform")}})"
    @echo "  postgres   → localhost:{{env_var_or_default("MEKHAN_PG_PORT", "15439")}}             (db: mekhan / user: mekhan / pw: mekhan)"
    @echo "  nats       → {{nats_url}}             (http monitor: {{nats_mon_url}})"
    @echo "  rustfs     → {{s3_endpoint}}      (console: http://localhost:{{s3_console_port}} / admin: rustfsadmin)"
    @echo "  vault      → {{vault_addr}}      (dev mode; root token: root; KV v2 at secret/)"
    @echo "  ollama     → http://localhost:11434      (shared across worktrees; native /api/chat)"
    @echo "  executor   → daemon (NATS-driven)        (cancel: http://localhost:{{cancel_port}})"
    @echo "  engine     → {{engine_url}}"
    @echo "  mekhan     → {{service_url}}"
    @echo "  app (dev)  → http://localhost:{{app_port}}"
    @echo ""
    @echo "  auth       → {{auth}}"
    @echo ""
    @echo "  logs   : just dev logs <ollama|executor|engine|mekhan|app|infra>"
    @echo "  status : just dev status"
    @echo "  stop   : just dev down"
    @echo "  wipe   : just dev reset   (destroys pg/nats/rustfs data)"
