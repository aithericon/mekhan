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
executor_features := "docker,python,http,http-cancel,file-ops,opendal-s3,url-inputs,llm,vllm,kreuzberg,smtp,postgres,loki,prometheus,ros,vault"

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
router_url   := env_var_or_default("MEKHAN_ROUTER_URL", "http://localhost:13200")
router_port  := env_var_or_default("MEKHAN_ROUTER_PORT", "13200")
ollama_port  := env_var_or_default("OLLAMA_PORT", "11434")
ollama_model := env_var_or_default("OLLAMA_MODEL", "qwen3.5:9b")
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
    @just dev::up-engine
    @just dev::_wait-engine-stream
    @just dev::up-mekhan
    @just dev::pg-demo-seed
    @just dev::up-router || true
    @just dev::up-executor || true
    @just dev::up-app
    @just dev::up-runner || true
    @just dev::up-model-runner || true
    @just dev::xarm-up || true
    @just dev::_summary

# Same as `up` but with auth ENABLED: mekhan-service runs in BFF mode (real
# Zitadel cookie login for the SPA + machine-PAT introspection for the CLI).
# Requires `bash deploy/zitadel/bootstrap.sh` once (writes mekhan.local.toml).
up-auth:
    @just dev::_dirs
    @just dev::_infra-up
    @just dev::_wait-infra
    @just dev::up-ollama
    @just dev::up-engine
    @just dev::_wait-engine-stream
    @just dev::up-mekhan bff
    @just dev::pg-demo-seed
    @just dev::up-router || true
    @just dev::up-executor || true
    @just dev::up-app
    @just dev::up-runner || true
    @just dev::up-model-runner || true
    @just dev::xarm-up || true
    @just dev::_summary "ENABLED (BFF + Zitadel PAT introspection)"

# Stop everything: native daemons first, then infra
down:
    @just dev::down-app      || true
    @just dev::_kill model-runner || true
    @just dev::_kill router  || true
    @just dev::down-mekhan   || true
    @just dev::down-engine   || true
    @just dev::down-executor || true
    @just dev::_kill runner  || true
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
    for name in ollama executor runner model-runner engine nomad router mekhan app; do
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
    # Clear persisted worker/runner ENROLLMENT identities. Enrollment state —
    # the workers/runners rows, their wkr_/rnr_ tokens, the scoped NATS creds,
    # AND the default-group `routing_partition` UUID the worker binds its grouped
    # consumer to — all live in the Postgres we just wiped. But each daemon caches
    # its identity in `{base_dir}/{worker,runner}/identity.json` and self-enroll
    # NO-OPS when that file is present (the intended `down`/`up` identity reuse).
    # After a DB WIPE that cache is STALE: the new `ensure_default_worker_group`
    # mints a FRESH default-group UUID, so a reused worker identity keeps binding a
    # `routing_partition` that resolves to no current capacity — its grouped
    # consumer (`executor-<wire>-grp.<prio>.<old_uuid>.>`) drains nothing and every
    # executor job hangs. Remove the identities so the next `up` re-enrolls against
    # the fresh DB and binds the current default-group partition.
    echo "▶ clearing stale worker/runner enrollment identities…"
    find "{{work_dir}}" -type d \( -name worker -o -name runner \) -prune -exec rm -rf {} + 2>/dev/null || true

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

    # Demo 36 (internal model pool) pins a SMALL, fast model so its degenerate
    # Agent inference reliably completes inside the template-test's 60s wall
    # clock even on a CPU-only / contended box (a 9B can take >60s there). ~1.3 GB,
    # pulled alongside the main model so `just dev` + `mekhan test
    # 36-internal-pool-agent` works out of the box. If you change this, also update
    # the model id in the demo's graph.json + the internal_pool_registry /
    # internal_pool_qwen fixtures. Override-free: independent of OLLAMA_MODEL.
    demo_model="${DEMO36_MODEL:-llama3.2:1b}"
    if ollama list 2>/dev/null | awk 'NR>1 {print $1}' | grep -Fxq "$demo_model"; then
      echo "  ✓ demo-36 model already present: $demo_model"
    else
      echo "▶ pulling demo-36 model: $demo_model  (~1.3 GB, small/fast for the live test)"
      ollama pull "$demo_model"
      echo "  ✓ pulled $demo_model"
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
# charlie).
#
# Runs automatically as a step of `up` (and therefore `reset`/`restart`), so a
# fresh volume always has the fixture and no separate seed command is needed.
# It lives in the pg volume, which `reset` wipes — without this the demo fails
# with `relation "demo_pg.widgets" does not exist`. Idempotent (TRUNCATE +
# re-INSERT), so re-running on every `up` just re-asserts the baseline. Still
# runnable standalone to reset the three rows.
#
# Uses the same {{database_url}} the rest of dev points at (slot-aware via env).
# The `demo_pg` resource the demo binds hardcodes the slot-0 port (15439); on a
# slotted worktree edit demos/resources/demo_pg.json to match before going live.
# `psql` is the only extra tool this needs; if it isn't on PATH the seed is
# skipped with a hint rather than failing the whole stack `up`.
pg-demo-seed:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v psql >/dev/null 2>&1; then
      echo "  · pg-demo-seed: psql not on PATH — skipping demo_pg.widgets fixture"
      echo "    (run \`just dev pg-demo-seed\` after installing libpq/psql if you need demo 19)"
      exit 0
    fi
    echo "▶ seeding demo_pg.widgets fixture into {{database_url}}"
    psql "{{database_url}}" -v ON_ERROR_STOP=1 -f demos/19-postgres-node/seed.sql
    echo "  ✓ demo_pg.widgets seeded (schema demo_pg, 3 baseline rows)"

# ── Executor (aithericon-executor-service, daemon mode) ──────────────────────
#
# The dev executor is the platform's WORKER. Workers are no longer anonymous:
# every worker enrolls into a worker GROUP and binds its consumer to that
# group's grouped stream (`executor-<wire>-grp/<partition>`), where the
# partition is the group's `capacity`-resource UUID. There is NO bare
# `executor-<wire>` dispatch path any more. So this recipe mirrors `up-runner`:
# it waits for mekhan + the workspace's auto-seeded "default" worker group,
# mints a worker registration token for group "default", and boots the executor
# with EXECUTOR_WORKER_REG_TOKEN set. The executor SELF-ENROLLS on boot
# (`maybe_enroll_worker`): POSTs /api/v1/workers/enroll, writes
# `{base_dir}/worker/identity.json` (carrying the wkr_ token + routing
# partition), and binds the grouped consumer. Identity reuse is automatic — the
# base_dir persists across a data-preserving `down`/`up`, and self-enroll
# no-ops when identity.json is already present.
#
# ⚠️⚠️  `just dev reset` IS REQUIRED after upgrading to the unified-worker
#       dispatch model.  Any demo AIR compiled before this change still routes
#       jobs to the now-RETIRED bare `executor-<wire>` stream, which no worker
#       consumes — those steps would HANG forever.  A `reset` recompiles every
#       seeded demo to the grouped `executor-<wire>-grp/<default-uuid>` stream
#       this enrolled worker actually drains.  ⚠️⚠️
#
# Wired into `up` AFTER mekhan (needs the control plane to mint a token +
# enroll), kept SOFT (`up-executor || true`) so a missing seed never wedges
# `just dev` — it warns + skips like up-runner.
#
# Worker daemon: enrolls into the "default" group, drains its grouped stream
up-executor:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs
    if [ -s "{{pid_dir}}/executor.pid" ] && kill -0 "$(cat {{pid_dir}}/executor.pid)" 2>/dev/null; then
      echo "  executor already running (pid $(cat {{pid_dir}}/executor.pid))"
      exit 0
    fi
    # Wait for mekhan to be ready AND to have seeded the workspace's "default"
    # worker group (a `capacity` resource at path "default" with the `worker`
    # preset). The worker MUST enroll into a real group — there is no anonymous
    # path — so the seed must exist before we mint a token. Soft-skip (exit 0)
    # if it never appears so `just dev` is never blocked by a worker that can't
    # enroll. `/api/v1/capacities` returns a bare array of {path, …}.
    echo "▶ waiting for mekhan + default worker-group seed…"
    ready=""
    for _ in $(seq 1 60); do
      if curl -fsS "{{service_url}}/api/v1/capacities" 2>/dev/null \
           | jq -e '.[]? | select(.path=="default")' >/dev/null 2>&1; then
        ready=1; break
      fi
      sleep 1
    done
    if [ -z "$ready" ]; then
      echo "  ⚠ mekhan/default worker-group seed not ready after 60s — skipping executor" >&2
      echo "    (no worker capacity until 'just dev up-executor'; AutomatedSteps would hang)" >&2
      exit 0
    fi
    echo "▶ building executor (features: {{executor_features}})…"
    (cd executor && cargo build -p aithericon-executor-service --features "{{executor_features}}")

    mkdir -p "{{work_dir}}/executor"
    # Write a fresh executor.toml each `up` so config drift can't accumulate.
    # base_dir is absolute so the run dir + runner script paths don't compound
    # with the executor's CWD when launching the subprocess. The base_dir is
    # STABLE across restarts, so the worker's `{base_dir}/worker/identity.json`
    # (written at first enroll) persists and self-enroll no-ops on `down`/`up`.
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

    # Mint a worker registration token for the "default" group ONLY when this
    # worker isn't already enrolled. The executor self-enroll is idempotent —
    # it skips when `{base_dir}/worker/identity.json` exists — but the
    # registration token is single-use server-side, so re-minting on every `up`
    # would leak tokens. So: reuse silently when identity.json is present, mint
    # a fresh token only when (re)enrolling (post-reset / first boot). We pass
    # the token + mekhan URL via env; the daemon's `maybe_enroll_worker` does
    # the POST /api/v1/workers/enroll + writes identity.json + binds the grouped
    # consumer to the returned routing partition (the group's capacity UUID).
    reg_token=""
    if [ -f "$exec_base/worker/identity.json" ]; then
      wid=$(jq -r '.worker_id // empty' "$exec_base/worker/identity.json" 2>/dev/null || true)
      echo "▶ reusing enrolled worker ${wid:-?} (identity.json present — no re-enroll)"
    else
      echo "▶ minting worker registration token (group=default)…"
      reg_token=$(curl -fsS -X POST "{{service_url}}/api/v1/workers/registration-tokens" \
          -H 'content-type: application/json' -d '{"group":"default"}' | jq -r '.token')
      if [ -z "$reg_token" ] || [ "$reg_token" = "null" ]; then
        echo "  ⚠ failed to mint worker registration token — skipping executor" >&2
        exit 0
      fi
      echo "  ✓ token minted; worker will self-enroll into group=default on boot"
    fi

    echo "▶ starting executor…"
    # CWD must contain executor.toml — executor loads `./executor.toml`.
    # Use bash -c so we can `cd` before exec inside nohup's child shell.
    # VAULT_ADDR is set so PlanSecretsHook can unwrap per-job wrapping tokens
    # the engine attaches; VAULT_TOKEN is intentionally NOT set on the
    # executor — it authenticates by exchanging the wrapping token itself.
    # EXECUTOR_MEKHAN_URL + EXECUTOR_WORKER_REG_TOKEN drive the boot-time
    # self-enroll into the "default" worker group (the reg token is empty when
    # already enrolled — the daemon then just reuses the on-disk identity).
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
        EXECUTOR_MEKHAN_URL="{{service_url}}" \
        EXECUTOR_WORKER_REG_TOKEN="$reg_token" \
        bash -c 'cd "{{work_dir}}/executor" && exec "$0"' "$bin")
    echo "  ✓ executor (pid $pid)  log: {{log_dir}}/executor.log  (worker group: default)"

down-executor:
    @just dev::_kill executor

# ── Lab-runner fleet (presence-pool runner, daemon mode) ─────────────────────
#
# Enrolls + starts ONE dev lab runner that joins the `lab_fleet` PRESENCE pool
# (seeded from demos/resources/lab_fleet.json) and advertises the `xrd`
# capability (demos/capability-types/xrd.json), so the presence-pool demos
# (26-runner-pool, 27-runner-xrd) can be granted a unit and run. This is wired
# into `just dev` (the `up` recipe) so the demos work out of the box — you don't
# need to run it by hand; it's exposed as `up-runner`/`down-runner` for manual
# restarts. It mints a registration token via mekhan's API, enrolls via
# `aithericon-executor register`, then launches a runner-mode executor daemon.
# `runner_id` self-discovers from the `identity.json` the enroll writes, so the
# daemon drains the SHARED `runner-jobs` stream partitioned to its own id and
# heartbeats `runner.{id}.presence`; mekhan's presence controller turns that
# heartbeat into one pool unit. Connects to the open dev NATS ANONYMOUSLY — the
# scoped runner JWT mekhan mints is written to disk but unused in dev (dev NATS
# has no auth). Re-runnable: each call stops the prior runner and enrolls a
# fresh, uniquely-named one; `just dev reset` wipes the rows. The runner uses its
# own base_dir (separate venvs/artifacts) and no HTTP cancel port (the
# worker-pool executor owns that), so it coexists with the default `executor`.
up-runner:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs
    # Wait for mekhan to be ready AND to have seeded the `lab_fleet` pool + the
    # `xrd` capability-type (the runner enrolls into the demo workspace and
    # advertises xrd — both must exist first). Soft-skip (exit 0) if they never
    # appear so `just dev` is never blocked by a runner that can't enroll.
    echo "▶ waiting for mekhan + lab-fleet seed…"
    ready=""
    for _ in $(seq 1 60); do
      if curl -fsS "{{service_url}}/api/v1/capability-types" 2>/dev/null \
           | jq -e '.items[]? | select(.name=="xrd")' >/dev/null 2>&1; then
        ready=1; break
      fi
      sleep 1
    done
    if [ -z "$ready" ]; then
      echo "  ⚠ mekhan/lab_fleet seed not ready after 60s — skipping lab runner" >&2
      echo "    (presence-pool demos 26/27 have no capacity until 'just dev up-runner')" >&2
      exit 0
    fi
    # Reuse the executor binary the worker-pool daemon built (same features).
    bin="${CARGO_TARGET_DIR:-$PWD/executor/target}/debug/aithericon-executor-service"
    if [ ! -x "$bin" ]; then
      echo "▶ building executor (features: {{executor_features}})…"
      (cd executor && cargo build -p aithericon-executor-service --features "{{executor_features}}")
    fi
    # Keep exactly one heartbeating unit: stop a previous dev runner.
    if [ -s "{{pid_dir}}/runner.pid" ] && kill -0 "$(cat {{pid_dir}}/runner.pid)" 2>/dev/null; then
      echo "  stopping previous runner (pid $(cat {{pid_dir}}/runner.pid))"
      just dev::_kill runner || true
    fi

    runner_base="$PWD/{{work_dir}}/runner-fleet"
    mkdir -p "$runner_base"
    # Runner-mode executor.toml. Distinct base_dir; [cancel] omitted so it
    # defaults to nats=true / http=false (no clash with the worker-pool
    # executor's HTTP cancel port); [metrics] omitted (optional). NATS is
    # anonymous — no `nats_creds`, so the open dev server accepts it. CRUCIAL:
    # `namespace` is LEFT UNSET so it stays at the `executor_jobs` sentinel —
    # config `normalize()` only rewrites it to the shared `runner-jobs` stream
    # (drained PARTITIONED to this runner id, where presence-pool grants route)
    # when it's still the sentinel. Pinning it here would suppress that override
    # and the runner would drain a dead namespace.
    cat > "$runner_base/executor.toml" <<EOF
    name        = "dev-lab-runner"
    nats_url    = "{{nats_url}}"
    base_dir    = "$runner_base"
    concurrency = 2

    [storage]
    backend  = "s3"
    endpoint = "{{s3_endpoint}}"
    bucket   = "mekhan-artifacts"
    region   = "us-east-1"

    [storage.credentials]
    access_key = "rustfsadmin"
    secret_key = "rustfsadmin"

    [logs]
    enabled = true
    sinks   = [{ type = "file" }, { type = "nats", min_level = "info" }]

    [python]
    enabled    = true
    prefer_uv  = true
    EOF

    # Reuse a STABLE runner identity across restarts. A data-preserving
    # `down`/`up` (or `restart`) keeps the runner row AND its pool unit, but
    # mekhan loses its in-memory presence map on restart and never reaps a unit
    # for a runner it no longer tracks. Enrolling a FRESH id each time would
    # then orphan the prior id's unit in the pool — and a claim granted to that
    # dead partition hangs (the demo times out). So if a local identity exists
    # and its runner row is still live, REUSE it: the same id re-acquires
    # (idempotent), so exactly one live unit owns the partition. Only re-enroll
    # when the row is gone (e.g. `just dev reset` wiped Postgres).
    identity="$runner_base/runner/identity.json"
    reuse=""
    if [ -f "$identity" ]; then
      rid=$(jq -r '.runner_id // empty' "$identity" 2>/dev/null || true)
      if [ -n "$rid" ] && curl -fsS "{{service_url}}/api/v1/runners/$rid" >/dev/null 2>&1; then
        echo "▶ reusing enrolled runner $rid (row still live)"
        reuse=1
      fi
    fi

    if [ -z "$reuse" ]; then
      rm -rf "$runner_base/runner"   # drop any stale identity (e.g. post-reset)
      echo "▶ minting registration token (pool=lab_fleet)…"
      rt=$(curl -fsS -X POST "{{service_url}}/api/v1/runners/registration-tokens" \
          -H 'content-type: application/json' -d '{"group":"lab_fleet"}' | jq -r '.token')
      if [ -z "$rt" ] || [ "$rt" = "null" ]; then
        echo "✗ failed to mint registration token (is the stack up? 'just dev status')" >&2
        exit 1
      fi
      name="dev-lab-$(date +%s)"
      echo "▶ enrolling runner '$name' (caps: xrd.max_2theta=180)…"
      ( cd "$runner_base" && env MEKHAN_URL="{{service_url}}" "$bin" register \
          --token "$rt" --name "$name" --pool lab_fleet \
          --capabilities '{"xrd":{"max_2theta":180,"source":"dev-diffractometer"}}' )
    fi

    echo "▶ starting lab runner daemon…"
    pid=$(just/scripts/spawn-daemon.sh runner "{{pid_dir}}/runner.pid" "{{log_dir}}/runner.log" \
      -- env RUST_LOG="info,aithericon_executor=debug" \
        VAULT_ADDR="{{vault_addr}}" \
        bash -c 'cd "'"$runner_base"'" && exec "$0"' "$bin")
    echo "  ✓ lab runner (pid $pid)  log: {{log_dir}}/runner.log"
    echo "  presence-pool demos now grantable: 26-runner-pool, 27-runner-xrd"

down-runner:
    @just dev::_kill runner

# ── Inference router (model-pool DATA plane, docs 11/29) ──────────────────────
#
# The OpenAI-compatible router that fronts the self-hosted model pool. Inference
# goes STRAIGHT here — never through the engine net or the presence net. The
# router admits per-engine `C`, meters each request to NATS (the inference-audit
# ledger the /models Router tab reads), and exposes the in-flight `/metrics`
# gauge mekhan scrapes for engine headroom (`AUTOSCALER_DEMAND_URL`, wired in
# up-mekhan → headroom-only; no model_policy is seeded, so the placement loop
# stays idle). Dev wires ONE upstream replica: the shared Ollama serving the
# default model over its OpenAI-compat surface. Re-runnable; skips if already up.
up-router:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs
    if [ -s "{{pid_dir}}/router.pid" ] && kill -0 "$(cat {{pid_dir}}/router.pid)" 2>/dev/null; then
      echo "  inference-router already running (pid $(cat {{pid_dir}}/router.pid))"
      exit 0
    fi
    echo "▶ building inference-router…"
    # Umbrella workspace member → output lands in ./target/, like mekhan-service.
    cargo build -p inference-router
    # One static upstream: the shared Ollama (OpenAI-compat) serving the default
    # model. `inventory.rs`'s live mekhan poll is a deferred upgrade; the static
    # ROUTER_REPLICAS table is the MVP source of truth.
    replicas='[{"base_url":"http://localhost:{{ollama_port}}","model_ids":["{{ollama_model}}"],"concurrency_c":4}]'
    echo "▶ starting inference-router on :{{router_port}} (upstream: ollama {{ollama_model}})…"
    pid=$(just/scripts/spawn-daemon.sh router "{{pid_dir}}/router.pid" "{{log_dir}}/router.log" \
      -- env \
        RUST_LOG="info,inference_router=debug" \
        ROUTER_BIND_ADDR="0.0.0.0:{{router_port}}" \
        ROUTER_NATS_URL="{{nats_url}}" \
        ROUTER_MEKHAN_URL="{{service_url}}" \
        ROUTER_REPLICAS="$replicas" \
        "${CARGO_TARGET_DIR:-$PWD/target}/debug/inference-router")
    echo "  ✓ inference-router (pid $pid)  {{router_url}}  log: {{log_dir}}/router.log"

down-router:
    @just dev::_kill router

# ── Model-pool node (the self-hosted LLM pool, docs 28-31) ────────────────────
#
# Enrolls + starts ONE dev model-server runner so the pool works OUT OF THE BOX:
# it appears on the /models Engines tab, and the shared Ollama's default model
# shows under "ready to load" (provisioned-to-disk, one click to make resident).
# A dedicated daemon — the faithful dev shape of a model-server NODE, kept
# distinct from the generic `executor` worker and the `lab_fleet` presence
# `runner`. Enrolls UNGROUPED (no seeded-group dependency; the model inventory
# join is group-agnostic) and runs the `[model_agent]` against the shared Ollama
# (backend=ollama; the dev executor binary now carries the `vllm` feature that
# gates the agent). Re-runnable with a STABLE identity (same reuse logic as
# up-runner). Soft-skips if mekhan never comes up.
up-model-runner:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs
    echo "▶ waiting for mekhan…"
    ready=""
    for _ in $(seq 1 60); do
      if curl -fsS "{{service_url}}/healthz" >/dev/null 2>&1; then ready=1; break; fi
      sleep 1
    done
    if [ -z "$ready" ]; then
      echo "  ⚠ mekhan not ready after 60s — skipping model-pool runner" >&2
      echo "    (the /models pool stays empty until 'just dev up-model-runner')" >&2
      exit 0
    fi
    # Reuse the executor binary (same {{executor_features}} — includes `vllm`,
    # which gates the model agent). Build if absent.
    bin="${CARGO_TARGET_DIR:-$PWD/executor/target}/debug/aithericon-executor-service"
    if [ ! -x "$bin" ]; then
      echo "▶ building executor (features: {{executor_features}})…"
      (cd executor && cargo build -p aithericon-executor-service --features "{{executor_features}}")
    fi
    if [ -s "{{pid_dir}}/model-runner.pid" ] && kill -0 "$(cat {{pid_dir}}/model-runner.pid)" 2>/dev/null; then
      echo "  stopping previous model runner (pid $(cat {{pid_dir}}/model-runner.pid))"
      just dev::_kill model-runner || true
    fi

    runner_base="$PWD/{{work_dir}}/model-runner"
    mkdir -p "$runner_base"
    # Model-server runner config. UNGROUPED (no [pool]); [model_agent] drives the
    # shared Ollama's admin surface (probe /api/ps + /api/tags, load/unload/pull).
    # `vllm_url` is the endpoint for whichever backend — here the shared Ollama.
    cat > "$runner_base/executor.toml" <<EOF
    name        = "dev-model-server"
    nats_url    = "{{nats_url}}"
    base_dir    = "$runner_base"
    concurrency = 2
    # The model agent + presence catalog publish need mekhan's URL to POST the
    # interface catalog (the live half of the /models Engines join).
    mekhan_url  = "{{service_url}}"

    [model_agent]
    backend           = "ollama"
    vllm_url          = "http://localhost:{{ollama_port}}"
    served_base_model = "{{ollama_model}}"
    max_num_seqs      = 4

    [storage]
    backend  = "s3"
    endpoint = "{{s3_endpoint}}"
    bucket   = "mekhan-artifacts"
    region   = "us-east-1"

    [storage.credentials]
    access_key = "rustfsadmin"
    secret_key = "rustfsadmin"

    [logs]
    enabled = true
    sinks   = [{ type = "file" }, { type = "nats", min_level = "info" }]
    EOF

    # Stable identity across restarts (same reasoning as up-runner): reuse the
    # enrolled id when its row is still live; only re-enroll after a reset wipes it.
    identity="$runner_base/runner/identity.json"
    reuse=""
    if [ -f "$identity" ]; then
      rid=$(jq -r '.runner_id // empty' "$identity" 2>/dev/null || true)
      if [ -n "$rid" ] && curl -fsS "{{service_url}}/api/v1/runners/$rid" >/dev/null 2>&1; then
        echo "▶ reusing enrolled model runner $rid (row still live)"
        reuse=1
      fi
    fi

    if [ -z "$reuse" ]; then
      rm -rf "$runner_base/runner"
      echo "▶ minting registration token (ungrouped)…"
      rt=$(curl -fsS -X POST "{{service_url}}/api/v1/runners/registration-tokens" \
          -H 'content-type: application/json' -d '{}' | jq -r '.token')
      if [ -z "$rt" ] || [ "$rt" = "null" ]; then
        echo "✗ failed to mint registration token (is the stack up? 'just dev status')" >&2
        exit 1
      fi
      name="dev-model-$(date +%s)"
      echo "▶ enrolling model runner '$name' (ungrouped, backend=ollama)…"
      ( cd "$runner_base" && env MEKHAN_URL="{{service_url}}" "$bin" register \
          --token "$rt" --name "$name" )
    fi

    echo "▶ starting model-pool runner daemon…"
    pid=$(just/scripts/spawn-daemon.sh model-runner "{{pid_dir}}/model-runner.pid" "{{log_dir}}/model-runner.log" \
      -- env RUST_LOG="info,aithericon_executor=debug" \
        VAULT_ADDR="{{vault_addr}}" \
        bash -c 'cd "'"$runner_base"'" && exec "$0"' "$bin")
    echo "  ✓ model-pool runner (pid $pid)  log: {{log_dir}}/model-runner.log"
    echo "  /models Engines now shows this node; '{{ollama_model}}' is ready to load."

down-model-runner:
    @just dev::_kill model-runner

# ── ROS turtle fleet (turtlesim + rosbridge container + ros runner) ──────────
#
# OPTIONAL dev layer (like scheduler-up): NOT wired into `just dev`. Brings up a
# headless turtlesim robot reachable over rosbridge, plus ONE `ros_fleet`
# presence-pool runner whose executor daemon points its ROS backend at that
# bridge — so the ros-backed demo (28-turtle-drive) can be granted a unit and
# run live end-to-end. Mirrors `up-runner` (identity reuse, presence pool,
# anonymous dev NATS) but joins `ros_fleet` and advertises the `ros`
# capability instead of `xrd`. Re-runnable: stops the prior container + runner.
#
# The rosbridge :9090 is mapped to a SLOT-AWARE host port derived from the
# worktree's port block: MEKHAN_SERVICE_PORT + 40 (slot 0 → 13140; slot N≥1 →
# base+40, e.g. slot 2 → 20240). The container is namespaced with the compose
# project prefix (mekhan-s<slot>) so two worktrees' turtles don't collide. The
# runner's executor.toml sets `[ros] ws_url = ws://localhost:<bridge>` and the
# flat `mekhan_url = <service_url>` (the EXECUTOR_MEKHAN_URL knob the runner-side
# interface-catalog publish reads).
ros-up:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs

    # Slot-aware host port for the rosbridge WebSocket. dev-ports.sh owns the
    # block; we derive +40 from the service port so it lands in this worktree's
    # block without a dev-ports.sh edit (slot 0 → 13140, slot N → base+40).
    bridge_port=$(( {{service_port}} + 40 ))
    # noVNC web viewer host port (+41) — open http://localhost:$vnc_port/vnc.html
    # to WATCH the turtle move while a ros-backed demo runs.
    vnc_port=$(( {{service_port}} + 41 ))
    # Namespace the container with the compose project (defaults to mekhan-s0
    # for the legacy/main checkout where COMPOSE_PROJECT_NAME is unset).
    proj="${COMPOSE_PROJECT_NAME:-mekhan-s0}"
    cname="${proj}-turtlesim"

    # 1) Build the turtlesim+rosbridge image if missing.
    if ! docker image inspect aithericon-turtlesim:jazzy >/dev/null 2>&1; then
      echo "▶ building turtlesim image (aithericon-turtlesim:jazzy)…"
      docker build -t aithericon-turtlesim:jazzy deploy/dev/ros
    fi

    # 2) (Re)start the turtle container on the slot-aware host ports (rosbridge
    #    WS + the noVNC web viewer).
    docker rm -f "$cname" >/dev/null 2>&1 || true
    echo "▶ starting turtlesim+rosbridge ($cname) → ws://localhost:$bridge_port"
    echo "  👁  watch the turtle: http://localhost:$vnc_port/vnc.html"
    docker run -d --name "$cname" -p "$bridge_port:9090" -p "$vnc_port:6080" aithericon-turtlesim:jazzy >/dev/null

    # 3) Wait for mekhan + the `ros` capability-type seed (same pattern as
    #    up-runner waits for `xrd`). Soft-skip if it never appears.
    echo "▶ waiting for mekhan + ros capability seed…"
    ready=""
    for _ in $(seq 1 60); do
      if curl -fsS "{{service_url}}/api/v1/capability-types" 2>/dev/null \
           | jq -e '.items[]? | select(.name=="ros")' >/dev/null 2>&1; then
        ready=1; break
      fi
      sleep 1
    done
    if [ -z "$ready" ]; then
      echo "  ⚠ mekhan/ros seed not ready after 60s — turtle container is up, but skipping ros runner" >&2
      echo "    (demo 28-turtle-drive has no capacity until 'just dev ros-up' enrolls a runner)" >&2
      exit 0
    fi

    # Reuse the executor binary the worker-pool daemon built (same features —
    # `ros` is in {{executor_features}}). Build if absent.
    bin="${CARGO_TARGET_DIR:-$PWD/executor/target}/debug/aithericon-executor-service"
    if [ ! -x "$bin" ]; then
      echo "▶ building executor (features: {{executor_features}})…"
      (cd executor && cargo build -p aithericon-executor-service --features "{{executor_features}}")
    fi

    # Keep exactly one heartbeating unit: stop a previous ros runner.
    if [ -s "{{pid_dir}}/ros-runner.pid" ] && kill -0 "$(cat {{pid_dir}}/ros-runner.pid)" 2>/dev/null; then
      echo "  stopping previous ros runner (pid $(cat {{pid_dir}}/ros-runner.pid))"
      just dev::_kill ros-runner || true
    fi

    runner_base="$PWD/{{work_dir}}/ros-fleet"
    mkdir -p "$runner_base"
    # Runner-mode executor.toml. Distinct base_dir. The ROS backend is
    # runner-local: `[ros] ws_url` points at the turtle container's host port.
    # The flat `mekhan_url` is the EXECUTOR_MEKHAN_URL knob the runner-side
    # interface catalog publish reads (POSTs to /api/v1/runners/{id}/interfaces). NATS is
    # anonymous (open dev server). `namespace` LEFT UNSET so config normalize()
    # rewrites it to the shared `runner-jobs` partition (where presence-pool
    # grants route) — see up-runner.
    cat > "$runner_base/executor.toml" <<EOF
    name        = "dev-ros-runner"
    nats_url    = "{{nats_url}}"
    base_dir    = "$runner_base"
    concurrency = 2
    # Flat top-level key (the EXECUTOR_MEKHAN_URL knob): the runner-side ROS
    # interface-catalog publish POSTs to {{service_url}}/api/v1/runners/{id}/interfaces.
    # Must precede any [section] — TOML scopes bare keys to the preceding table.
    mekhan_url  = "{{service_url}}"

    [storage]
    backend  = "s3"
    endpoint = "{{s3_endpoint}}"
    bucket   = "mekhan-artifacts"
    region   = "us-east-1"

    [storage.credentials]
    access_key = "rustfsadmin"
    secret_key = "rustfsadmin"

    [logs]
    enabled = true
    sinks   = [{ type = "file" }, { type = "nats", min_level = "info" }]

    [python]
    enabled    = true
    prefer_uv  = true

    [ros]
    ws_url = "ws://localhost:$bridge_port"
    EOF

    # Reuse a STABLE runner identity across restarts (see up-runner for the full
    # rationale — a fresh id each time orphans the prior pool unit and hangs a
    # grant). Reuse when the local identity exists AND its runner row is live.
    identity="$runner_base/runner/identity.json"
    reuse=""
    if [ -f "$identity" ]; then
      rid=$(jq -r '.runner_id // empty' "$identity" 2>/dev/null || true)
      if [ -n "$rid" ] && curl -fsS "{{service_url}}/api/v1/runners/$rid" >/dev/null 2>&1; then
        echo "▶ reusing enrolled ros runner $rid (row still live)"
        reuse=1
      fi
    fi

    if [ -z "$reuse" ]; then
      rm -rf "$runner_base/runner"   # drop any stale identity (e.g. post-reset)
      echo "▶ minting registration token (pool=ros_fleet)…"
      rt=$(curl -fsS -X POST "{{service_url}}/api/v1/runners/registration-tokens" \
          -H 'content-type: application/json' -d '{"group":"ros_fleet"}' | jq -r '.token')
      if [ -z "$rt" ] || [ "$rt" = "null" ]; then
        echo "✗ failed to mint registration token (is the stack up? 'just dev status')" >&2
        exit 1
      fi
      name="dev-ros-$(date +%s)"
      echo "▶ enrolling runner '$name' (caps: ros.robot_model=turtlesim, ros.ros_distro=jazzy)…"
      ( cd "$runner_base" && env MEKHAN_URL="{{service_url}}" "$bin" register \
          --token "$rt" --name "$name" --pool ros_fleet \
          --capabilities '{"ros":{"robot_model":"turtlesim","ros_distro":"jazzy"}}' )
    fi

    echo "▶ starting ros runner daemon…"
    pid=$(just/scripts/spawn-daemon.sh ros-runner "{{pid_dir}}/ros-runner.pid" "{{log_dir}}/ros-runner.log" \
      -- env RUST_LOG="info,aithericon_executor=debug" \
        VAULT_ADDR="{{vault_addr}}" \
        bash -c 'cd "'"$runner_base"'" && exec "$0"' "$bin")
    echo "  ✓ ros runner (pid $pid)  log: {{log_dir}}/ros-runner.log"
    echo "  turtlesim: ws://localhost:$bridge_port  ($cname)"
    echo "  ros-backed demo now grantable: 28-turtle-drive"

# Tear down the ROS layer: stop+rm the turtle container, stop the ros runner
# daemon. Leaves the rest of the dev stack untouched.
ros-down:
    #!/usr/bin/env bash
    set -euo pipefail
    proj="${COMPOSE_PROJECT_NAME:-mekhan-s0}"
    cname="${proj}-turtlesim"
    echo "▶ stopping turtle container ($cname)…"
    docker rm -f "$cname" >/dev/null 2>&1 || true
    just dev::_kill ros-runner || true
    echo "  ✓ ROS layer down (rest of stack untouched)"

# ── xArm 6 fleet (fake-hardware xArm sim + rosbridge + xarm runner) ───────────
#
# OPTIONAL dev layer, sibling of `ros-up`: a SIMULATED UFACTORY xArm 6 (ROS 2
# Jazzy, ros2_control fake hardware + MoveIt + the xarm_planner services)
# reachable over rosbridge, plus ONE `xarm_fleet` presence-pool runner whose
# executor points its ROS backend at that bridge — so the xarm demo
# (30-xarm-joint-move) can be granted a unit and run live. Identical mechanism
# to `ros-up`, differing only in image / group / capability values, so it can
# run ALONGSIDE the turtle layer (distinct ports, container, runner, group).
#
# The rosbridge :9090 maps to MEKHAN_SERVICE_PORT + 42 (the turtle uses +40/+41)
# so both fleets coexist in one worktree's port block. Same `ros` capability
# type as the turtle — the xArm just advertises `robot_model=xarm6`.
#
# Heavy first run: `xarm-up` builds aithericon-xarm:jazzy if missing, which
# colcon-builds xarm_ros2 + MoveIt (multi-GB, several minutes; cached after).
xarm-up:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs

    # Slot-aware rosbridge host port (+42; turtle owns +40/+41).
    bridge_port=$(( {{service_port}} + 42 ))
    # noVNC web viewer host port (+43) — open http://localhost:$vnc_port/vnc.html
    # to WATCH the 3D arm (RViz) execute the demo trajectories.
    vnc_port=$(( {{service_port}} + 43 ))
    proj="${COMPOSE_PROJECT_NAME:-mekhan-s0}"
    cname="${proj}-xarm"

    # Slot-aware DDS domain. The image bakes ROS_DOMAIN_ID=42 to isolate the
    # xArm from a default-domain (0) turtle in the SAME checkout. But Docker's
    # bridge passes DDS multicast across containers, so two worktrees both
    # running `xarm-up` would put two domain-42 xArm stacks on the bridge and
    # cross-discover each other (duplicate move_group / controllers → flaky
    # execution + polluted catalog). Bump the domain per slot so each worktree's
    # arm is isolated. Slot 0 (main checkout) stays 42 — back-compatible with an
    # already-running slot-0 container; slot N → 42+N.
    slot="${proj##*-s}"; [[ "$slot" =~ ^[0-9]+$ ]] || slot=0
    ros_domain=$(( 42 + slot ))

    # 1) Build the xArm fake-mode image if missing (heavy — see header).
    if ! docker image inspect aithericon-xarm:jazzy >/dev/null 2>&1; then
      echo "▶ building xArm image (aithericon-xarm:jazzy) — colcon-builds xarm_ros2 + MoveIt, several minutes…"
      docker build -t aithericon-xarm:jazzy deploy/dev/xarm
    fi

    # 2) (Re)start the xArm container on the slot-aware host ports (rosbridge WS
    #    + the noVNC web viewer: RViz with the MoveIt MotionPlanning display).
    docker rm -f "$cname" >/dev/null 2>&1 || true
    echo "▶ starting xArm 6 (fake hw) + rosbridge ($cname) → ws://localhost:$bridge_port (DDS domain $ros_domain)"
    echo "  👁  watch the 3D arm: http://localhost:$vnc_port/vnc.html"
    echo "  (the planner stack — move_group + controllers + RViz — takes ~30-60s to come up)"
    docker run -d --name "$cname" -e ROS_DOMAIN_ID="$ros_domain" -p "$bridge_port:9090" -p "$vnc_port:6080" aithericon-xarm:jazzy >/dev/null

    # 3) Wait for mekhan + the `ros` capability-type seed (shared with the turtle).
    echo "▶ waiting for mekhan + ros capability seed…"
    ready=""
    for _ in $(seq 1 60); do
      if curl -fsS "{{service_url}}/api/v1/capability-types" 2>/dev/null \
           | jq -e '.items[]? | select(.name=="ros")' >/dev/null 2>&1; then
        ready=1; break
      fi
      sleep 1
    done
    if [ -z "$ready" ]; then
      echo "  ⚠ mekhan/ros seed not ready after 60s — xArm container is up, but skipping xarm runner" >&2
      echo "    (demo 30-xarm-joint-move has no capacity until 'just dev xarm-up' enrolls a runner)" >&2
      exit 0
    fi

    # Reuse the executor binary the worker-pool / ros daemons built (same features
    # — `ros` is in {{executor_features}}). Build if absent.
    bin="${CARGO_TARGET_DIR:-$PWD/executor/target}/debug/aithericon-executor-service"
    if [ ! -x "$bin" ]; then
      echo "▶ building executor (features: {{executor_features}})…"
      (cd executor && cargo build -p aithericon-executor-service --features "{{executor_features}}")
    fi

    # Keep exactly one heartbeating unit: stop a previous xarm runner.
    if [ -s "{{pid_dir}}/xarm-runner.pid" ] && kill -0 "$(cat {{pid_dir}}/xarm-runner.pid)" 2>/dev/null; then
      echo "  stopping previous xarm runner (pid $(cat {{pid_dir}}/xarm-runner.pid))"
      just dev::_kill xarm-runner || true
    fi

    runner_base="$PWD/{{work_dir}}/xarm-fleet"
    mkdir -p "$runner_base"
    # Runner-mode executor.toml. The ROS backend is runner-local: `[ros] ws_url`
    # points at the xArm container's host port. Flat `mekhan_url` is the
    # EXECUTOR_MEKHAN_URL knob the catalog publish reads. `namespace` LEFT UNSET
    # so config normalize() rewrites it to the shared `runner-jobs` partition.
    cat > "$runner_base/executor.toml" <<EOF
    name        = "dev-xarm-runner"
    nats_url    = "{{nats_url}}"
    base_dir    = "$runner_base"
    concurrency = 2
    mekhan_url  = "{{service_url}}"

    [storage]
    backend  = "s3"
    endpoint = "{{s3_endpoint}}"
    bucket   = "mekhan-artifacts"
    region   = "us-east-1"

    [storage.credentials]
    access_key = "rustfsadmin"
    secret_key = "rustfsadmin"

    [logs]
    enabled = true
    sinks   = [{ type = "file" }, { type = "nats", min_level = "info" }]

    [python]
    enabled    = true
    prefer_uv  = true

    [ros]
    ws_url = "ws://localhost:$bridge_port"
    EOF

    # Reuse a STABLE runner identity across restarts (see ros-up / up-runner for
    # the rationale). Reuse when the local identity exists AND its row is live.
    identity="$runner_base/runner/identity.json"
    reuse=""
    if [ -f "$identity" ]; then
      rid=$(jq -r '.runner_id // empty' "$identity" 2>/dev/null || true)
      if [ -n "$rid" ] && curl -fsS "{{service_url}}/api/v1/runners/$rid" >/dev/null 2>&1; then
        echo "▶ reusing enrolled xarm runner $rid (row still live)"
        reuse=1
      fi
    fi

    if [ -z "$reuse" ]; then
      rm -rf "$runner_base/runner"   # drop any stale identity (e.g. post-reset)
      echo "▶ minting registration token (pool=xarm_fleet)…"
      rt=$(curl -fsS -X POST "{{service_url}}/api/v1/runners/registration-tokens" \
          -H 'content-type: application/json' -d '{"group":"xarm_fleet"}' | jq -r '.token')
      if [ -z "$rt" ] || [ "$rt" = "null" ]; then
        echo "✗ failed to mint registration token (is the stack up? 'just dev status')" >&2
        exit 1
      fi
      name="dev-xarm-$(date +%s)"
      echo "▶ enrolling runner '$name' (caps: ros.robot_model=xarm6, ros.ros_distro=jazzy)…"
      ( cd "$runner_base" && env MEKHAN_URL="{{service_url}}" "$bin" register \
          --token "$rt" --name "$name" --pool xarm_fleet \
          --capabilities '{"ros":{"robot_model":"xarm6","ros_distro":"jazzy"}}' )
    fi

    echo "▶ starting xarm runner daemon…"
    pid=$(just/scripts/spawn-daemon.sh xarm-runner "{{pid_dir}}/xarm-runner.pid" "{{log_dir}}/xarm-runner.log" \
      -- env RUST_LOG="info,aithericon_executor=debug" \
        VAULT_ADDR="{{vault_addr}}" \
        bash -c 'cd "'"$runner_base"'" && exec "$0"' "$bin")
    echo "  ✓ xarm runner (pid $pid)  log: {{log_dir}}/xarm-runner.log"
    echo "  xArm 6 (fake): ws://localhost:$bridge_port  ($cname)"
    echo "  ros-backed demo now grantable: 30-xarm-joint-move"

# Tear down the xArm layer: stop+rm the xArm container, stop the xarm runner
# daemon. Leaves the rest of the dev stack (and the turtle layer) untouched.
xarm-down:
    #!/usr/bin/env bash
    set -euo pipefail
    proj="${COMPOSE_PROJECT_NAME:-mekhan-s0}"
    cname="${proj}-xarm"
    echo "▶ stopping xArm container ($cname)…"
    docker rm -f "$cname" >/dev/null 2>&1 || true
    just dev::_kill xarm-runner || true
    echo "  ✓ xArm layer down (rest of stack untouched)"

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
    # musl + static (docs/22 container staging): the binary is bind-mounted into
    # apptainer images, so it must not depend on the image's glibc. The slim
    # feature set links no C libs (shells out to python3, rustls for HTTP), so a
    # fully static musl build is clean. zigbuild handles the musl C toolchain.
    echo "▶ cross-compiling executor for x86_64-unknown-linux-musl (static, slim features)…"
    (cd executor && cargo zigbuild -p aithericon-executor-service \
       --features "python,http,http-cancel,opendal-s3,url-inputs" \
       --target x86_64-unknown-linux-musl)
    exec_bin="$PWD/executor/target/x86_64-unknown-linux-musl/debug/aithericon-executor-service"
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

    # 3a-bis. Container staging (docs/22): ensure the shared-FS stand-in dirs the
    #    materialize effect + the apptainer-wrapped drain executor use exist and
    #    are world-writable (the Dockerfile creates them on a fresh build; this is
    #    an idempotent belt-and-suspenders for a cached image). Then sanity-check
    #    apptainer + the bind-mounted static `uv` are present — both come from the
    #    Dockerfile, so a miss means the image predates the container-staging
    #    layer (re-run is a `--build`, so it self-heals on the next slurm-up).
    docker exec "$container" mkdir -p /shared/sif/by-ref /shared/venv-cache /shared/apptainer-cache
    docker exec "$container" chmod -R 0777 /shared
    docker exec "$container" sh -c 'command -v apptainer >/dev/null 2>&1' \
      || echo "  ⚠ apptainer not found in the Slurm container — container-staging e2e will fail (rebuild: just dev slurm-down && slurm-up)"
    docker exec "$container" test -x /opt/petri/bin/uv \
      || echo "  ⚠ /opt/petri/bin/uv missing in the Slurm container — in-container venv builds will fail"
    echo "  ✓ /shared dirs ready; apptainer + uv checked (container staging, docs/22)"

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
        AUTOSCALER_DEMAND_URL="{{router_url}}" \
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
    @echo "  lab runner → daemon (presence pool)      (lab_fleet · demos 26/27 · 'just dev down-runner')"
    @echo "  engine     → {{engine_url}}"
    @echo "  mekhan     → {{service_url}}"
    @echo "  app (dev)  → http://localhost:{{app_port}}"
    @echo ""
    @echo "  auth       → {{auth}}"
    @echo ""
    @echo "  logs   : just dev logs <ollama|executor|runner|engine|mekhan|app|infra>"
    @echo "  status : just dev status"
    @echo "  stop   : just dev down"
    @echo "  wipe   : just dev reset   (destroys pg/nats/rustfs data)"
