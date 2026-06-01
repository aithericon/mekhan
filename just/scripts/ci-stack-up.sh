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

# =============================================================================


export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

# ── Compiler cache (sccache) ─────────────────────────────────────────────────
# CARGO_TARGET_DIR caches compiled artifacts, but Woodpecker re-clones the repo
# every run → fresh source mtimes → cargo's mtime-based fingerprint rebuilds our
# OWN crates (executor/crates/*, shared/*, mekhan, engine) even when the bytes
# are identical. sccache keys on source CONTENT, so those become cache hits
# across clones. CARGO_INCREMENTAL=0 is required (sccache won't cache
# incremental artifacts). All of this is opportunistic: any failure leaves the
# build working, just without the extra cache.
_cache_root="$(dirname "${CARGO_HOME:-$HOME/.cargo}")"   # /var/lib/ci-jobs/cache in CI
_sccache=""
if command -v sccache >/dev/null 2>&1; then
  _sccache="$(command -v sccache)"
else
  # Fetch a static musl binary ONCE into the persistent cache dir.
  _bin="$_cache_root/bin/sccache"
  if [ -x "$_bin" ]; then
    _sccache="$_bin"
  else
    _ver="v0.8.2"
    _arch="$(uname -m)"                                  # aarch64 on the worker
    _pkg="sccache-${_ver}-${_arch}-unknown-linux-musl"
    echo "  · fetching sccache ${_ver} (${_arch})…"
    mkdir -p "$_cache_root/bin"
    if curl -fsSL "https://github.com/mozilla/sccache/releases/download/${_ver}/${_pkg}.tar.gz" \
         | tar -xz -C "$_cache_root/bin" --strip-components=1 "${_pkg}/sccache" 2>/dev/null \
       && chmod +x "$_bin"; then
      _sccache="$_bin"
    else
      echo "  · sccache fetch failed — building without it" >&2
    fi
  fi
fi
if [ -n "$_sccache" ]; then
  export RUSTC_WRAPPER="$_sccache"
  export SCCACHE_DIR="${SCCACHE_DIR:-$_cache_root/sccache}"
  export SCCACHE_CACHE_SIZE="${SCCACHE_CACHE_SIZE:-20G}"
  export CARGO_INCREMENTAL=0
  "$_sccache" --start-server >/dev/null 2>&1 || true
  echo "  · sccache: $_sccache (SCCACHE_DIR=$SCCACHE_DIR)"
fi

# Deterministic, per-pipeline project name → predictable network name and
# clean teardown. CI_PIPELINE_NUMBER is set by Woodpecker; falls back for local.
: "${COMPOSE_PROJECT_NAME:=mekhan-ci-${CI_PIPELINE_NUMBER:-local}}"
export COMPOSE_PROJECT_NAME
_ci_net="${COMPOSE_PROJECT_NAME}_default"


docker compose -p "$COMPOSE_PROJECT_NAME" down -v --remove-orphans >/dev/null 2>&1 || true

echo "▶ infra up (postgres + nats + vault + rustfs) — project ${COMPOSE_PROJECT_NAME}…"
docker compose -p "$COMPOSE_PROJECT_NAME" up -d --remove-orphans postgres nats vault rustfs

_self="$(grep -oE 'containers/[0-9a-f]{64}' /proc/self/mountinfo 2>/dev/null | head -1 | cut -d/ -f2 || true)"
[ -n "$_self" ] || _self="$(cat /etc/hostname 2>/dev/null || hostname)"
echo "  · step container id: ${_self:-<unknown>}"
# Surface the real docker error (no 2>/dev/null) and fail fast — a silent skip
# here just turns into a confusing 60s DNS timeout later.
if docker network connect "$_ci_net" "$_self"; then
  echo "  ✓ joined compose network ${_ci_net}"
else
  rc=$?

  echo "  · docker network connect returned $rc — verifying DNS…" >&2
fi


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
