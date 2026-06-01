#!/usr/bin/env bash
# =============================================================================
# CI integration/e2e stack — infra bring-up + network wiring + env export
# =============================================================================
# MUST be SOURCED (it `export`s into the caller's shell), not executed:
#
#     source just/scripts/ci-stack-up.sh
#
# Sourced by `just ci::test-integration` and `just ci::test-e2e`. Assumes
# CWD = repo root and the caller has `set -euo pipefail`.
#
# WHY this shape (vs. the local `just dev up` path):
#   The Woodpecker worker uses the DOCKER backend (WOODPECKER_BACKEND=docker)
#   and mounts the host /var/run/docker.sock. Pipeline steps therefore run as
#   containers on a per-pipeline Woodpecker network, while `docker compose`
#   (launched here via the mounted socket) starts the infra as SIBLING
#   containers on their own compose network. The two can't see each other over
#   `localhost`, and the docker backend doesn't expose host networking to the
#   pipeline YAML.
#
#   The fix: bring the infra up from the PROVEN docker-compose.yml (it already
#   encodes nats `--jetstream`, vault `-dev`, and the rustfs creds — none of
#   which a Woodpecker `services:` block can express, since services take no
#   command/entrypoint), then attach THIS step's container to the compose
#   network so the services resolve by their compose DNS names (`postgres`,
#   `nats`, `vault`, `rustfs`). Every endpoint mekhan/engine/executor read is
#   env-overridable (MEKHAN_* in just/dev.just), so we point them — and the
#   TEST_* harness vars — at those in-network hostnames.
#
# NOTE: the `docker network connect <self>` step is the one piece that can only
# be validated on the real worker (step-container id detection + socket perms).
# If it ever fails, the readiness loops below time out with a clear message.
# =============================================================================

# Deterministic, per-pipeline project name → predictable network name and
# clean teardown. CI_PIPELINE_NUMBER is set by Woodpecker; falls back for local.
: "${COMPOSE_PROJECT_NAME:=mekhan-ci-${CI_PIPELINE_NUMBER:-local}}"
export COMPOSE_PROJECT_NAME
_ci_net="${COMPOSE_PROJECT_NAME}_default"

echo "▶ infra up (postgres + nats + vault + rustfs) — project ${COMPOSE_PROJECT_NAME}…"
docker compose -p "$COMPOSE_PROJECT_NAME" up -d postgres nats vault rustfs

# Attach the running step container to the compose network for DNS access.
# The step's container id is the 64-hex string in its own mountinfo (overlay
# upperdir path); `hostname` is the fallback (docker sets it to the short id).
_self="$(grep -m1 -oE '[0-9a-f]{64}' /proc/self/mountinfo 2>/dev/null | head -c 12 || true)"
[ -n "$_self" ] || _self="$(hostname)"
if docker network connect "$_ci_net" "$_self" 2>/dev/null; then
  echo "  ✓ joined compose network ${_ci_net} as ${_self}"
else
  echo "  · network connect skipped (already attached, or running with host net)"
fi

# Endpoints — compose-internal DNS names + CONTAINER ports (not host ports).
# The daemons we start in-step (engine/executor/mekhan) bind on localhost, so
# the *_URL that point at them stay localhost; only the infra moves to DNS.
export MEKHAN_DATABASE_URL="postgres://mekhan:mekhan@postgres:5432/mekhan"
export MEKHAN_NATS_URL="nats://nats:4222"
export MEKHAN_NATS_MON_URL="http://nats:8222"
export MEKHAN_S3_ENDPOINT="http://rustfs:9000"
export MEKHAN_VAULT_ADDR="http://vault:8200"
export MEKHAN_ENGINE_URL="http://localhost:13030"
export MEKHAN_SERVICE_URL="http://localhost:13100"

# Test harness reads its own TEST_* vars (service/tests/common/test_infra.rs).
export TEST_POSTGRES_URL="$MEKHAN_DATABASE_URL"
export TEST_NATS_URL="$MEKHAN_NATS_URL"
export TEST_S3_ENDPOINT="$MEKHAN_S3_ENDPOINT"
export TEST_ENGINE_URL="$MEKHAN_ENGINE_URL"
export TEST_PETRI_URL="$MEKHAN_ENGINE_URL"

_wait() { # name url
  echo "▶ waiting for $1…"
  for _ in $(seq 1 60); do
    if curl -sf "$2" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  echo "✗ $1 not ready at $2 after 60s" >&2
  return 1
}

echo "▶ waiting for postgres…"
for _ in $(seq 1 60); do
  docker compose -p "$COMPOSE_PROJECT_NAME" exec -T postgres \
    pg_isready -U mekhan -d mekhan >/dev/null 2>&1 && break
  sleep 1
done
_wait nats   "$MEKHAN_NATS_MON_URL/healthz"
_wait vault  "$MEKHAN_VAULT_ADDR/v1/sys/health?standbyok=true"
_wait rustfs "$MEKHAN_S3_ENDPOINT/"
echo "✓ infra ready (DNS: postgres / nats / vault / rustfs)"
