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

# Executor build features. `llm` is included so an AutomatedStep with
# backendType=llm can dispatch against the local Ollama daemon
# (`just dev ollama-up`); external-provider workflows need API keys in the
# LlmConfig but the local Ollama path needs none. `kreuzberg` is
# included so demo 07 (OCR + LLM classification) works out of the box —
# first build pulls heavy pdfium deps, but the cached rebuild cost is
# normal.
executor_features := "docker,python,http,http-cancel,file-ops,opendal-s3,url-inputs,llm,kreuzberg"

# Local infra endpoints (also baked into service/mekhan.toml + engine env)
nats_url     := "nats://localhost:4333"
database_url := "postgres://mekhan:mekhan@localhost:5439/mekhan"
s3_endpoint  := "http://localhost:9005"

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

_infra-up:
    @echo "▶ infra up (postgres + nats + rustfs)…"
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

# scheduler="nomad" additionally wires the engine to a Nomad scheduler
# backend so `Scheduled` AutomatedSteps (which bridge to the long-lived
# `scheduler-net`) can actually dispatch. Default "off" keeps dev minimal
# (the plain `up` path is unchanged). Use `just dev scheduler-up` rather
# than calling this directly with nomad — it also stands up the agent,
# registers the job template, and deploys scheduler-net/executor-net.
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
    # only needs the scheduler env to register the scheduler_submit handler.
    # Exported (not -i) so the `env` invocation below inherits them.
    if [ "{{scheduler}}" = "nomad" ]; then
      echo "  ↳ scheduler: Nomad (SCHEDULER_BACKEND=nomad → scheduler-net)"
      export SCHEDULER_BACKEND="nomad"
      export NOMAD_ADDR="http://127.0.0.1:4646"
      export SCHEDULER_JOB_TEMPLATE="petri-executor-worker"
      export SCHEDULER_SIGNAL_ROUTES="running:sig_running,completed:sig_completed,failed:sig_failed"
    fi
    if [ "{{scheduler}}" = "slurm" ]; then
      # Slurm reaches the cluster over SSH against the local docker-compose
      # cluster (engine/infra/slurm/), polls squeue/sacct, and publishes
      # signals to scheduler-net via NATS. Reverse the whole layer with
      # `just dev slurm-down`.
      echo "  ↳ scheduler: Slurm (SCHEDULER_BACKEND=slurm → scheduler-net)"
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

# ── Nomad scheduler layer (for `Scheduled` AutomatedSteps) ───────────────────
#
# Stands up the real-Nomad dispatch path on top of a running dev stack so a
# `Scheduled` AutomatedStep can actually run. The compiled parent net bridges
# to the long-lived `scheduler-net`:
#
#   parent-net ─▶ scheduler-net ─(Nomad dispatch: petri-executor-worker)─▶
#                 executor-net  ─▶ executor ─▶ result ─▶ back to parent-net
#
# Requires `just dev up` already running (reuses its NATS/rustfs + executor
# binary) and the `nomad` binary on PATH. Restarts the engine with the
# scheduler env. Reverse with `just dev scheduler-down`. Modeled on the
# engine's `just demo three-layer` recipe, minus job-net (mekhan's compiled
# parent net replaces it).
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
    exec_bin="$PWD/executor/target/debug/aithericon-executor-service"
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
          }' engine/infra/nomad/executor-job-template.json \
      | curl -sf -X POST http://127.0.0.1:4646/v1/jobs -d @- >/dev/null
    echo "  ✓ petri-executor-worker registered (binary: $exec_bin)"

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
    # lived per dispatch and respawned per scheduler_submit, so killing them
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

    # 5. Build infra nets, then restart the engine WITH the scheduler env.
    echo "▶ building scheduler-net + executor-net examples…"
    (cd engine && cargo build -p aithericon-sdk --example scheduler_net --example executor_net)
    just dev::down-engine
    just dev::up-engine nomad
    echo "▶ waiting for engine API…"
    for _ in $(seq 1 30); do
      curl -sf http://localhost:3030/api/nets/metadata >/dev/null 2>&1 && break
      sleep 1
    done
    curl -sf http://localhost:3030/api/nets/metadata >/dev/null 2>&1 || {
      echo "✗ engine did not come back (log: {{log_dir}}/engine.log)" >&2; exit 1; }
    sleep 2  # let the Nomad + executor watchers connect

    # 5. Deploy BOTH infra nets, THEN activate (every bridge target must be
    #    deployed before either net transitions to Running — Strict gate).
    echo "▶ deploying + activating executor-net & scheduler-net…"
    cd engine
    ./target/debug/examples/executor_net --bridged --process --deploy --net-id executor-net >/dev/null
    ./target/debug/examples/scheduler_net --deploy --net-id scheduler-net >/dev/null
    ./target/debug/aithericon activate executor-net
    ./target/debug/aithericon activate scheduler-net

    echo ""
    echo "✓ Nomad scheduler layer up:"
    echo "  nomad        → http://localhost:4646/ui"
    echo "  scheduler-net + executor-net deployed & running on :3030"
    echo "  engine now started with SCHEDULER_BACKEND=nomad (Scheduled steps dispatch)"
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

# ── Slurm scheduler layer (for `Scheduled` AutomatedSteps via Slurm) ─────────
#
# Mirror of `scheduler-up` but using the local docker-compose Slurm cluster
# (engine/infra/slurm/) instead of Nomad. Net topology is identical — the
# parent's `bridge_out` still targets `scheduler-net/job_inbox`; only the
# backend that scheduler-net dispatches to changes:
#
#   parent-net ─▶ scheduler-net ─(sbatch over SSH: mekhan-executor-worker)─▶
#                 executor-net  ─▶ executor (in Slurm job) ─▶ result ─▶ parent
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
    export EXECUTOR_NATS_URL=nats://host.docker.internal:4333
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
    export EXECUTOR_STORAGE__ENDPOINT=http://host.docker.internal:9005
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

    # 5. Build infra nets, then restart the engine WITH the Slurm scheduler env.
    echo "▶ building scheduler-net + executor-net examples…"
    (cd engine && cargo build -p aithericon-sdk --example scheduler_net --example executor_net)
    just dev::down-engine
    just dev::up-engine slurm
    echo "▶ waiting for engine API…"
    for _ in $(seq 1 30); do
      curl -sf http://localhost:3030/api/nets/metadata >/dev/null 2>&1 && break
      sleep 1
    done
    curl -sf http://localhost:3030/api/nets/metadata >/dev/null 2>&1 || {
      echo "✗ engine did not come back (log: {{log_dir}}/engine.log)" >&2; exit 1; }
    sleep 2  # let the Slurm + executor watchers connect

    # 6. Deploy BOTH infra nets, THEN activate (Strict bridge gate).
    echo "▶ deploying + activating executor-net & scheduler-net…"
    cd engine
    ./target/debug/examples/executor_net --bridged --process --deploy --net-id executor-net >/dev/null
    ./target/debug/examples/scheduler_net --deploy --net-id scheduler-net >/dev/null
    ./target/debug/aithericon activate executor-net
    ./target/debug/aithericon activate scheduler-net

    echo ""
    echo "✓ Slurm scheduler layer up:"
    echo "  slurm        → docker compose (login: ssh -p 2222 testuser@localhost)"
    echo "  scheduler-net + executor-net deployed & running on :3030"
    echo "  engine now started with SCHEDULER_BACKEND=slurm (Scheduled steps dispatch)"
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
      echo "▶ starting mekhan-service on :3100 (auth: BFF + Zitadel PAT introspection)…"
    else
      mode_assign="MEKHAN__AUTH__MODE=dev_noop"
      echo "▶ starting mekhan-service on :3100 (auth: dev_noop — offline)…"
    fi
    nohup env \
      RUST_LOG="info,mekhan_service=debug" \
      MEKHAN__DATABASE_URL="{{database_url}}" \
      MEKHAN__NATS_URL="{{nats_url}}" \
      MEKHAN__PETRI_LAB_URL="http://localhost:3030" \
      MEKHAN__S3__ENDPOINT="{{s3_endpoint}}" \
      MEKHAN__S3__ACCESS_KEY="rustfsadmin" \
      MEKHAN__S3__SECRET_KEY="rustfsadmin" \
      MEKHAN__DEMOS__SEED="true" \
      MEKHAN__DEMOS__DIR="$PWD/demos" \
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
    @echo "  ollama     → http://localhost:11434      (native /api/chat; up if installed, see just/dev.just::up-ollama)"
    @echo "  executor   → daemon (NATS-driven)        (cancel: http://localhost:3105)"
    @echo "  engine     → http://localhost:3030"
    @echo "  mekhan     → http://localhost:3100"
    @echo "  app (dev)  → http://localhost:5173"
    @echo ""
    @echo "  auth       → {{auth}}"
    @echo ""
    @echo "  logs   : just dev logs <ollama|executor|engine|mekhan|app|infra>"
    @echo "  status : just dev status"
    @echo "  stop   : just dev down"
    @echo "  wipe   : just dev reset   (destroys pg/nats/rustfs data)"
