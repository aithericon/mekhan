#!/usr/bin/env bash
# Idempotent infrastructure provisioning for E2E integration tests.
#
# Ensures all services are running and healthy:
#   1. Docker containers (NATS + Postgres)
#   2. petri-lab engine (with executor feature + all effect handlers)
#   3. aithericon-executor (real executor service)
#   4. mekhan-service
#
# Usage:
#   ./tests/e2e/scripts/ensure-infra.sh          # Start everything
#   ./tests/e2e/scripts/ensure-infra.sh --stop    # Stop everything
#
# The script is idempotent: running it multiple times is safe.
# It checks health endpoints first and only starts services that aren't running.

set -euo pipefail

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MEKHAN_APP_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
MEKHAN_ROOT="$(cd "$MEKHAN_APP_DIR/.." && pwd)"
MEKHAN_SERVICE_DIR="$MEKHAN_ROOT/service"
PETRI_LAB_DIR="$(cd "$MEKHAN_ROOT/../petri-lab" && pwd)"
EXECUTOR_REPO="$(cd "$MEKHAN_ROOT/../aithericon-executor" && pwd)"

HPI_DIR="$(cd "$MEKHAN_ROOT/../aithericon-human-ui" && pwd)"

BACKEND_URL="http://localhost:3100"
PETRI_URL="http://localhost:3030"
HPI_URL="http://localhost:5188"

# PID files for background processes
PID_DIR="/tmp/mekhan-e2e-pids"
mkdir -p "$PID_DIR"

HEALTH_TIMEOUT=30  # seconds

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

log() { echo "[ensure-infra] $*"; }

is_port_open() {
    lsof -ti :"$1" > /dev/null 2>&1
}

wait_for_url() {
    local url="$1" label="$2" timeout="${3:-$HEALTH_TIMEOUT}"
    for i in $(seq 1 "$timeout"); do
        if curl -sf "$url" > /dev/null 2>&1; then
            log "$label is ready"
            return 0
        fi
        if [ "$i" -eq "$timeout" ]; then
            log "ERROR: $label did not become healthy within ${timeout}s"
            return 1
        fi
        sleep 1
    done
}

kill_pid_file() {
    local pidfile="$1" label="$2"
    if [ -f "$pidfile" ]; then
        local pid
        pid=$(cat "$pidfile")
        if kill -0 "$pid" 2>/dev/null; then
            log "Stopping $label (PID $pid)..."
            kill "$pid" 2>/dev/null || true
            sleep 1
        fi
        rm -f "$pidfile"
    fi
}

# ---------------------------------------------------------------------------
# Stop mode
# ---------------------------------------------------------------------------

if [ "${1:-}" = "--stop" ]; then
    log "Stopping all services..."
    kill_pid_file "$PID_DIR/mekhan-service.pid" "mekhan-service"
    kill_pid_file "$PID_DIR/hpi.pid" "HPI"
    kill_pid_file "$PID_DIR/executor.pid" "aithericon-executor"
    kill_pid_file "$PID_DIR/petri-lab.pid" "petri-lab"
    docker compose -f "$MEKHAN_ROOT/docker-compose.yml" down 2>/dev/null || true
    log "All services stopped."
    exit 0
fi

# ---------------------------------------------------------------------------
# Prerequisites
# ---------------------------------------------------------------------------

if [ ! -f "$EXECUTOR_REPO/Cargo.toml" ]; then
    log "ERROR: aithericon-executor repo not found at $EXECUTOR_REPO"
    log "Expected sibling checkout: git clone <executor-repo> $EXECUTOR_REPO"
    exit 1
fi

# ---------------------------------------------------------------------------
# 1. Docker containers (NATS + Postgres)
# ---------------------------------------------------------------------------

log "Checking Docker containers (NATS + Postgres)..."

docker_healthy=true
# Check NATS
if ! curl -sf http://localhost:8222/healthz > /dev/null 2>&1; then
    docker_healthy=false
fi
# Check Postgres
if ! pg_isready -h localhost -p 5432 -U mekhan -d mekhan > /dev/null 2>&1; then
    docker_healthy=false
fi

if [ "$docker_healthy" = true ]; then
    log "Docker containers already healthy"
else
    log "Starting Docker containers..."
    docker compose -f "$MEKHAN_ROOT/docker-compose.yml" up -d --wait
    log "Docker containers started"
fi

# ---------------------------------------------------------------------------
# 2. petri-lab engine (with executor feature + all effect handlers)
# ---------------------------------------------------------------------------

log "Checking petri-lab engine..."

if curl -sf "$PETRI_URL/api/nets" > /dev/null 2>&1; then
    log "petri-lab already running"
else
    # Kill stale process on port 3030 if any
    if is_port_open 3030; then
        log "Killing stale process on port 3030..."
        kill $(lsof -ti :3030) 2>/dev/null || true
        sleep 1
    fi
    kill_pid_file "$PID_DIR/petri-lab.pid" "stale petri-lab"

    log "Starting petri-lab with executor feature..."
    (
        cd "$PETRI_LAB_DIR"
        # Load executor env from petri-lab's infra config
        set -a
        source infra/executor/executor.env
        set +a
        RUST_LOG=info,petri_executor=debug,petri_nats=debug \
        cargo run -p core-engine --all-features > /tmp/petri-lab-e2e.log 2>&1 &
        echo $! > "$PID_DIR/petri-lab.pid"
    )

    wait_for_url "$PETRI_URL/api/nets" "petri-lab" 60
    # Give watchers time to connect
    sleep 3
fi

# ---------------------------------------------------------------------------
# 3. aithericon-executor (real executor service)
# ---------------------------------------------------------------------------

log "Checking aithericon-executor..."

executor_running=false
if [ -f "$PID_DIR/executor.pid" ]; then
    pid=$(cat "$PID_DIR/executor.pid")
    if kill -0 "$pid" 2>/dev/null; then
        executor_running=true
    fi
fi
# Also check if any aithericon-executor-service process is running
if pgrep -f "aithericon-executor-service" > /dev/null 2>&1; then
    executor_running=true
fi

if [ "$executor_running" = true ]; then
    log "aithericon-executor already running"
else
    kill_pid_file "$PID_DIR/executor.pid" "stale executor"

    log "Starting aithericon-executor..."
    (
        cd "$EXECUTOR_REPO"
        # Load executor env from petri-lab's infra config
        set -a
        source "$PETRI_LAB_DIR/infra/executor/aithericon-executor.env"
        set +a
        RUST_LOG=info \
        ./target/debug/aithericon-executor-service > /tmp/executor-e2e.log 2>&1 &
        echo $! > "$PID_DIR/executor.pid"
    )

    # Give executor time to connect and create NATS consumer
    sleep 3
    log "aithericon-executor started"
fi

# ---------------------------------------------------------------------------
# 4. HPI (aithericon-human-ui)
# ---------------------------------------------------------------------------

log "Checking HPI..."

if curl -sf "$HPI_URL" > /dev/null 2>&1; then
    log "HPI already running"
else
    # Kill stale process on port 5188 if any
    if is_port_open 5188; then
        log "Killing stale process on port 5188..."
        kill $(lsof -ti :5188) 2>/dev/null || true
        sleep 1
    fi
    kill_pid_file "$PID_DIR/hpi.pid" "stale HPI"

    log "Starting HPI..."
    (
        cd "$HPI_DIR"
        HPI_NATS_SOURCES=true \
        HPI_DEFAULT_ORG=default \
        NATS_URL="nats://localhost:4222" \
        DB_BACKEND=sqlite \
        AUTH_DB_PATH="/tmp/hpi-e2e.db" \
        ADMIN_EMAIL="system@hpi.dev" \
        ADMIN_PASSWORD="test1234" \
        ORIGIN="http://localhost:5188" \
        deno task dev > /tmp/hpi-e2e.log 2>&1 &
        echo $! > "$PID_DIR/hpi.pid"
    )

    wait_for_url "$HPI_URL" "HPI" 30
fi

# ---------------------------------------------------------------------------
# 5. mekhan-service
# ---------------------------------------------------------------------------

log "Checking mekhan-service..."

if curl -sf "$BACKEND_URL/api/templates?page=1&per_page=1" > /dev/null 2>&1; then
    log "mekhan-service already running"
else
    # Kill stale process on port 3100 if any
    if is_port_open 3100; then
        log "Killing stale process on port 3100..."
        kill $(lsof -ti :3100) 2>/dev/null || true
        sleep 1
    fi
    kill_pid_file "$PID_DIR/mekhan-service.pid" "stale mekhan-service"

    log "Starting mekhan-service..."
    (
        cd "$MEKHAN_SERVICE_DIR"
        MEKHAN_DATABASE_URL="postgres://mekhan:mekhan@localhost:5432/mekhan" \
        MEKHAN_PETRI_LAB_URL="http://localhost:3030" \
        MEKHAN_NATS_URL="nats://localhost:4222" \
        RUST_LOG=info \
        cargo run > /tmp/mekhan-service-e2e.log 2>&1 &
        echo $! > "$PID_DIR/mekhan-service.pid"
    )

    wait_for_url "$BACKEND_URL/api/templates?page=1&per_page=1" "mekhan-service" 60
fi

# ---------------------------------------------------------------------------
# Final health check
# ---------------------------------------------------------------------------

log ""
log "=== Infrastructure Status ==="

all_healthy=true
for check in \
    "NATS:http://localhost:8222/healthz" \
    "petri-lab:$PETRI_URL/api/nets" \
    "HPI:$HPI_URL" \
    "mekhan-service:$BACKEND_URL/api/templates?page=1&per_page=1"; do
    label="${check%%:*}"
    url="${check#*:}"
    if curl -sf "$url" > /dev/null 2>&1; then
        log "  $label: OK"
    else
        log "  $label: FAILED"
        all_healthy=false
    fi
done

# Check executor by PID
if [ -f "$PID_DIR/executor.pid" ] && kill -0 "$(cat "$PID_DIR/executor.pid")" 2>/dev/null; then
    log "  aithericon-executor: OK (PID $(cat "$PID_DIR/executor.pid"))"
elif pgrep -f "aithericon-executor-service" > /dev/null 2>&1; then
    log "  aithericon-executor: OK (external process)"
else
    log "  aithericon-executor: FAILED"
    all_healthy=false
fi

if [ "$all_healthy" = true ]; then
    log ""
    log "All services healthy. Ready for E2E tests."
    exit 0
else
    log ""
    log "Some services are not healthy. Check logs in /tmp/*-e2e.log"
    exit 1
fi
