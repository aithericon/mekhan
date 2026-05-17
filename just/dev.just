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

# Executor build features. Skip `kreuzberg` (heavy pdfium deps) and `llm`
# (requires API keys) — re-add if you need them.
executor_features := "docker,python,http,http-cancel,file-ops,opendal-s3,url-inputs"

# Local infra endpoints (also baked into service/mekhan.toml + engine env)
nats_url     := "nats://localhost:4333"
database_url := "postgres://mekhan:mekhan@localhost:5439/mekhan"
s3_endpoint  := "http://localhost:9005"

# ── Entrypoints ──────────────────────────────────────────────────────────────

# Default recipe: bring up the full stack
default: up

# Start everything: infra + executor + engine + mekhan + app (auth: dev_noop)
up:
    @just dev::_dirs
    @just dev::_infra-up
    @just dev::_wait-infra
    @just dev::up-executor
    @just dev::up-engine
    @just dev::up-mekhan
    @just dev::up-app
    @just dev::_summary

# Same as `up` but with auth ENABLED: mekhan-service runs in BFF mode (real
# Zitadel cookie login for the SPA + machine-PAT introspection for the CLI).
# Requires `bash deploy/zitadel/bootstrap.sh` once (writes mekhan.local.toml).
up-auth:
    @just dev::_dirs
    @just dev::_infra-up auth
    @just dev::_wait-infra
    @just dev::up-executor
    @just dev::up-engine
    @just dev::up-mekhan bff
    @just dev::up-app
    @just dev::_summary "ENABLED (BFF + Zitadel PAT introspection)"

# Stop everything: native daemons first, then infra
down:
    @just dev::down-app      || true
    @just dev::down-mekhan   || true
    @just dev::down-engine   || true
    @just dev::down-executor || true
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
    for name in executor engine mekhan app; do
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

# Regenerate openapi-mekhan.json (from the mekhan-service binary, no infra
# needed) and refresh the SvelteKit app's typed schema from it. Run this
# after any change to a #[utoipa::path] annotation or ToSchema-derived DTO.
openapi:
    #!/usr/bin/env bash
    set -euo pipefail
    echo "▶ building mekhan CLI…"
    cargo build --bin mekhan
    echo "▶ dumping spec → openapi-mekhan.json"
    ./target/debug/mekhan openapi > openapi-mekhan.json
    echo "▶ regenerating app/src/lib/api/schema.d.ts"
    (cd app && pnpm run openapi:generate)
    echo "✓ openapi spec + schema regenerated"

# ── Infra ────────────────────────────────────────────────────────────────────

# Bring docker-compose infra up. Optional `profile` arg passes
# `--profile <profile>` to docker compose, used by `up-auth` to include
# the `auth`-gated Zitadel containers (zitadel-db / zitadel / zitadel-login
# / zitadel-proxy). Default (no profile) skips them — the standard
# `just dev up` flow runs mekhan-service in `dev_noop` mode and does not
# need Zitadel.
_infra-up profile="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{profile}}" ]; then
      echo "▶ infra up (postgres + nats + rustfs + profile: {{profile}})…"
      docker compose --profile {{profile}} up -d
    else
      echo "▶ infra up (postgres + nats + rustfs)…"
      docker compose up -d
    fi

# Tear infra down. `docker compose down` stops all containers in the
# project regardless of which profile started them, so no profile arg
# is needed here.
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
    project="$(basename "$PWD")"
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
      curl -sf http://localhost:8333/healthz >/dev/null 2>&1 && break
      sleep 1
    done
    echo "▶ waiting for rustfs…"
    for _ in $(seq 1 30); do
      curl -sf "{{s3_endpoint}}/" >/dev/null 2>&1 && break
      sleep 1
    done

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
    http_port = 3105
    EOF

    echo "▶ starting executor…"
    # CWD must contain executor.toml — executor loads `./executor.toml`.
    # Use bash -c so we can `cd` before exec inside nohup's child shell.
    bin="$PWD/executor/target/debug/aithericon-executor-service"
    nohup env RUST_LOG="info,aithericon_executor=debug" \
      bash -c 'cd "{{work_dir}}/executor" && exec "$0"' "$bin" \
      > "{{log_dir}}/executor.log" 2>&1 &
    echo $! > "{{pid_dir}}/executor.pid"
    echo "  ✓ executor (pid $(cat {{pid_dir}}/executor.pid))  log: {{log_dir}}/executor.log"

down-executor:
    @just dev::_kill executor

# ── Engine (engine/core-engine) ──────────────────────────────────────────────

up-engine:
    #!/usr/bin/env bash
    set -euo pipefail
    just dev::_dirs
    if [ -s "{{pid_dir}}/engine.pid" ] && kill -0 "$(cat {{pid_dir}}/engine.pid)" 2>/dev/null; then
      echo "  engine already running (pid $(cat {{pid_dir}}/engine.pid))"
      exit 0
    fi
    echo "▶ building engine…"
    (cd engine && cargo build -p core-engine)

    echo "▶ starting engine on :3030…"
    nohup env \
      RUST_LOG="info,petri_application=debug,petri_api=debug" \
      NATS_URL="{{nats_url}}" \
      EXECUTOR_NATS_URL="{{nats_url}}" \
      EXECUTOR_ENABLED="true" \
      EXECUTOR_NAMESPACE="executor" \
      PORT="3030" \
      "$PWD/engine/target/debug/core-engine" \
      > "{{log_dir}}/engine.log" 2>&1 &
    echo $! > "{{pid_dir}}/engine.pid"
    echo "  ✓ engine (pid $(cat {{pid_dir}}/engine.pid))  http://localhost:3030  log: {{log_dir}}/engine.log"

down-engine:
    @just dev::_kill engine

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
      echo "▶ starting mekhan-service on :3100 (auth: BFF + Zitadel PAT introspection)…"
    else
      mode_assign="MEKHAN__AUTH__MODE=dev_noop"
      echo "▶ starting mekhan-service on :3100 (auth: dev_noop — offline)…"
    fi
    # CLOUD_LAYER_JWT_SECRET propagation (sub-phase 2.4): mekhan-service's
    # cloud-layer BFF proxy mints HS256 JWTs against this secret for
    # service-to-service calls to cloud-layer-workflow. When unset, the
    # proxy endpoints fail-closed with 503 "cloud-layer not configured"
    # (legitimate; mekhan-service still works for native mekhan-NATS flows).
    # Conditional pass-through: include the env line only when the parent
    # shell has it exported. Operator runs `CLOUD_LAYER_JWT_SECRET=... just
    # dev up` or sources cloud-layer's .env.dev before launching.
    nohup env \
      RUST_LOG="info,mekhan_service=debug" \
      MEKHAN__DATABASE_URL="{{database_url}}" \
      MEKHAN__NATS_URL="{{nats_url}}" \
      MEKHAN__PETRI_LAB_URL="http://localhost:3030" \
      MEKHAN__S3__ENDPOINT="{{s3_endpoint}}" \
      MEKHAN__S3__ACCESS_KEY="rustfsadmin" \
      MEKHAN__S3__SECRET_KEY="rustfsadmin" \
      ${CLOUD_LAYER_JWT_SECRET:+CLOUD_LAYER_JWT_SECRET="$CLOUD_LAYER_JWT_SECRET"} \
      ${CLOUD_LAYER_BASE_URL:+CLOUD_LAYER_BASE_URL="$CLOUD_LAYER_BASE_URL"} \
      ${mode_assign:+"$mode_assign"} \
      "$PWD/target/debug/mekhan-service" \
      > "{{log_dir}}/mekhan.log" 2>&1 &
    echo $! > "{{pid_dir}}/mekhan.pid"
    echo "  ✓ mekhan-service (pid $(cat {{pid_dir}}/mekhan.pid))  http://localhost:3100  log: {{log_dir}}/mekhan.log"

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
    echo "▶ starting SvelteKit dev server on :5173…"
    nohup bash -c 'cd "$PWD/app" && exec pnpm run dev -- --host 0.0.0.0 --port 5173' \
      > "{{log_dir}}/app.log" 2>&1 &
    echo $! > "{{pid_dir}}/app.pid"
    echo "  ✓ app (pid $(cat {{pid_dir}}/app.pid))  http://localhost:5173  log: {{log_dir}}/app.log"

down-app:
    @just dev::_kill app

# ── Internal helpers ─────────────────────────────────────────────────────────

_dirs:
    @mkdir -p "{{pid_dir}}" "{{log_dir}}" "{{work_dir}}"

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
    @echo "✓ dev stack up:"
    @echo "  postgres   → localhost:5439              (db: mekhan / user: mekhan / pw: mekhan)"
    @echo "  nats       → localhost:4333              (http monitor: http://localhost:8333)"
    @echo "  rustfs     → http://localhost:9005       (console: http://localhost:9006 / admin: rustfsadmin)"
    @echo "  executor   → daemon (NATS-driven)        (cancel: http://localhost:3105)"
    @echo "  engine     → http://localhost:3030"
    @echo "  mekhan     → http://localhost:3100"
    @echo "  app (dev)  → http://localhost:5173"
    @echo ""
    @echo "  auth       → {{auth}}"
    @echo ""
    @echo "  logs   : just dev logs <executor|engine|mekhan|app|infra>"
    @echo "  status : just dev status"
    @echo "  stop   : just dev down"
    @echo "  wipe   : just dev reset   (destroys pg/nats/rustfs data)"
