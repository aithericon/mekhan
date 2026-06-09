#!/usr/bin/env bash
# =============================================================================
# CI cargo build env — linker memory budget + sccache
# =============================================================================
# MUST be SOURCED (it `export`s into the caller's shell), not executed:
#
#     source just/scripts/ci-cargo-env.sh
#
# Sourced by EVERY CI lane that compiles Rust: the `test-unit-rust` recipe
# directly, and the integration/e2e lanes via ci-stack-up.sh. Keeping it in one
# place means no lane can accidentally miss these and OOM.
#
# WHY: builds run on the capped ARM CI node (mekhan-arm pool / stateful-2,
# limit_mem = 6 GiB). Linking debug test binaries there OOM-kills `ld`
# (`ld terminated with signal 9`). Knobs:
#   • debuginfo=0 on dev+test profiles — debug objects dominate link memory.
#   • CARGO_BUILD_JOBS=2 — cap concurrent links (raise/lower via step env).
#   • sccache — content-addressed compile cache; makes our OWN crates cache-hit
#     across Woodpecker's fresh per-run clones (cargo's mtime fingerprint
#     otherwise rebuilds them). CARGO_INCREMENTAL=0 required.
# All opportunistic: any failure here still leaves a working (slower) build.
# =============================================================================

export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"

# sccache root derives from CARGO_HOME so it lands on the same cache volume
# (/woodpecker/cache in CI → sccache at /woodpecker/cache/sccache).
_cache_root="$(dirname "${CARGO_HOME:-$HOME/.cargo}")"
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
