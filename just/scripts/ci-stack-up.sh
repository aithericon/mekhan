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

# ── Linker memory budget ─────────────────────────────────────────────────────
# The CI node is a small autoscaled `stateless` box. `cargo test --workspace`
# links DOZENS of integration-test binaries (each service/tests/*.rs is its own
# crate that statically links the whole mekhan-service + deps — 257+ object
# files), and running several `ld` invocations at once OOM-killed the linker
# (`ld terminated with signal 9`). Two knobs keep peak RSS in budget:
#   • debuginfo=0 on the dev+test profiles — debug object files dominate link
#     memory; dropping them shrinks every link (we don't need gdb in CI).
#   • CARGO_BUILD_JOBS=1 — one rustc/link at a time, so peak memory is a single
#     link (the daemon builds, which link one-at-a-time, already proved that
#     fits). Override by exporting CARGO_BUILD_JOBS before the recipe if the
#     node is bigger.
# These are exported into the caller's shell, so the daemon builds (up-engine /
# up-mekhan / up-executor) AND `cargo test` all see them — one consistent set of
# flags keeps the shared CARGO_TARGET_DIR cache coherent (no rebuild thrash).
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"

# Deterministic, per-pipeline project name → predictable network name and
# clean teardown. CI_PIPELINE_NUMBER is set by Woodpecker; falls back for local.
: "${COMPOSE_PROJECT_NAME:=mekhan-ci-${CI_PIPELINE_NUMBER:-local}}"
export COMPOSE_PROJECT_NAME
_ci_net="${COMPOSE_PROJECT_NAME}_default"

# Defensive: clear any leftover stack with this project name (e.g. a prior
# hard-killed run, or the integration lane's stack before e2e reuses the same
# project sequentially). Cheap no-op when nothing is there.
docker compose -p "$COMPOSE_PROJECT_NAME" down -v --remove-orphans >/dev/null 2>&1 || true

echo "▶ infra up (postgres + nats + vault + rustfs) — project ${COMPOSE_PROJECT_NAME}…"
docker compose -p "$COMPOSE_PROJECT_NAME" up -d --remove-orphans postgres nats vault rustfs

# Attach THIS step's container to the compose network so the services resolve
# by their compose DNS name. Reliable self-id: /etc/hostname, /etc/hosts and
# /etc/resolv.conf are bind-mounted from the host path
# /var/lib/docker/containers/<CONTAINER-ID>/..., so the 64-hex after
# `containers/` in our OWN mountinfo is our container id on the host daemon —
# unlike the overlay layer ids, which a bare `[0-9a-f]{64}` grep would catch.
_self="$(grep -oE 'containers/[0-9a-f]{64}' /proc/self/mountinfo 2>/dev/null | head -1 | cut -d/ -f2 || true)"
[ -n "$_self" ] || _self="$(cat /etc/hostname 2>/dev/null || hostname)"
echo "  · step container id: ${_self:-<unknown>}"
# Surface the real docker error (no 2>/dev/null) and fail fast — a silent skip
# here just turns into a confusing 60s DNS timeout later.
if docker network connect "$_ci_net" "$_self"; then
  echo "  ✓ joined compose network ${_ci_net}"
else
  rc=$?
  # rc!=0 can also mean "already attached" (idempotent re-run); only hard-fail
  # if the infra DNS genuinely doesn't resolve.
  echo "  · docker network connect returned $rc — verifying DNS…" >&2
fi

# Hard gate: confirm a service name actually resolves from this step before we
# waste time on readiness loops. Catches a wrong self-id / failed attach.
# Guarded on getent — if it's absent we skip the gate and let the readiness
# loops below surface any DNS failure (just more slowly).
if command -v getent >/dev/null 2>&1; then
  for _ in $(seq 1 10); do
    getent hosts nats >/dev/null 2>&1 && break
    sleep 1
  done
  if ! getent hosts nats >/dev/null 2>&1; then
    echo "✗ 'nats' does not resolve from this step — network attach failed (id='${_self}', net='${_ci_net}')" >&2
    echo "  attached networks for this container:" >&2
    docker inspect -f '{{range $k,$v := .NetworkSettings.Networks}}{{$k}} {{end}}' "$_self" 2>/dev/null >&2 || true
    exit 1
  fi
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

_wait() { # name url [mode]
  # mode=ok (default): require a 2xx/3xx (curl -f). mode=connect: ready as soon
  # as the port answers HTTP at all — used for the S3 API, which returns 403 on
  # `/` without auth (a 403 still means rustfs is up and serving).
  local mode="${3:-ok}" flag="-sf"
  [ "$mode" = "connect" ] && flag="-s"
  echo "▶ waiting for $1…"
  for _ in $(seq 1 60); do
    if curl $flag -o /dev/null "$2" 2>/dev/null; then return 0; fi
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
_wait rustfs "$MEKHAN_S3_ENDPOINT/" connect
echo "✓ infra ready (DNS: postgres / nats / vault / rustfs)"
