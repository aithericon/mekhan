#!/usr/bin/env bash
# Single source of truth for per-worktree dev-stack ports + compose project.
#
# WHY: `just dev` brings up a fixed-port stack (postgres 15439, nats 14333,
# mekhan 13100, …). Running it from two worktrees at once collides on host
# ports AND docker container/volume names, so concurrent integration testing
# fights itself. This script maps a per-worktree slot integer to a private,
# predictable port block + a private compose project, so every worktree gets
# its own isolated stack.
#
# USAGE — meant to be *sourced* (it `export`s into the caller's env):
#     export WORKTREE_SLOT=3
#     source just/scripts/dev-ports.sh
# `.envrc` does exactly this so direnv loads the slot's env into every shell;
# `just dev` recipes then read the exported vars (with legacy fallbacks, so
# the recipes still work outside direnv / in CI).
#
# Standalone (debug what a slot resolves to):
#     WORKTREE_SLOT=3 bash just/scripts/dev-ports.sh --print
#
# SCHEME:
#   slot 0  → legacy ports (unchanged: 13100/13030/14333/15439/…), and NO
#             COMPOSE_PROJECT_NAME override — preserves the main checkout's
#             existing containers, volumes, docs, and muscle memory.
#   slot N≥1 → a private 100-wide block at  base = 20000 + N*100.
#             Each service has a fixed sub-offset within the block, so the
#             last two digits identify the service and the middle digits the
#             slot (slot 3 → mekhan 20300, engine 20301, pg 20310, …).
#             100-wide blocks from 20000 leave room for ~450 slots before
#             the 16-bit port ceiling — far more than the worktree count.
#
# Ollama (11434) is intentionally NOT slotted: model downloads are heavy and
# serving is read-only, so all worktrees share one daemon.
#
# NOTE: deliberately no `set -euo pipefail` — this script is sourced into
# interactive/direnv shells, where those options would leak and bite the
# caller. Errors are handled explicitly with `return || exit`.

slot="${WORKTREE_SLOT:-0}"
if ! [[ "$slot" =~ ^[0-9]+$ ]]; then
    echo "dev-ports.sh: WORKTREE_SLOT must be a non-negative integer, got '$slot'" >&2
    return 1 2>/dev/null || exit 1
fi

if [[ "$slot" -eq 0 ]]; then
    # ── Legacy block: byte-for-byte the historical fixed ports. ──────────────
    MEKHAN_SERVICE_PORT=13100
    MEKHAN_ENGINE_PORT=13030
    MEKHAN_CANCEL_PORT=13105
    MEKHAN_APP_PORT=15173
    MEKHAN_PG_PORT=15439
    MEKHAN_NATS_PORT=14333
    MEKHAN_NATS_MON_PORT=18333
    MEKHAN_VAULT_PORT=18200
    MEKHAN_S3_PORT=19005
    MEKHAN_S3_CONSOLE_PORT=19006
    MEKHAN_ZITADEL_DB_PORT=15440
    MEKHAN_ZITADEL_PORT=18080
    MEKHAN_MAILPIT_SMTP_PORT=1025
    MEKHAN_MAILPIT_UI_PORT=8025
    MEKHAN_HTTPBIN_PORT=13110
    MEKHAN_LIVEKIT_PORT=7880
    MEKHAN_LIVEKIT_RTC_TCP_PORT=7881
    MEKHAN_LIVEKIT_RTC_UDP_PORT=7882
    # No COMPOSE_PROJECT_NAME override — compose defaults to the dir basename
    # (aithericon-platform), matching the pre-existing stack's volumes.
else
    base=$(( 20000 + slot * 100 ))
    if [[ "$base" -gt 65400 ]]; then
        echo "dev-ports.sh: WORKTREE_SLOT=$slot exceeds the addressable range" >&2
        return 1 2>/dev/null || exit 1
    fi
    # Sub-offsets — last two digits identify the service within the block.
    MEKHAN_SERVICE_PORT=$(( base + 0 ))
    MEKHAN_ENGINE_PORT=$(( base + 1 ))
    MEKHAN_CANCEL_PORT=$(( base + 2 ))
    MEKHAN_APP_PORT=$(( base + 3 ))
    MEKHAN_PG_PORT=$(( base + 10 ))
    MEKHAN_NATS_PORT=$(( base + 11 ))
    MEKHAN_NATS_MON_PORT=$(( base + 12 ))
    MEKHAN_VAULT_PORT=$(( base + 13 ))
    MEKHAN_S3_PORT=$(( base + 14 ))
    MEKHAN_S3_CONSOLE_PORT=$(( base + 15 ))
    MEKHAN_ZITADEL_DB_PORT=$(( base + 20 ))
    MEKHAN_ZITADEL_PORT=$(( base + 21 ))
    MEKHAN_MAILPIT_SMTP_PORT=$(( base + 30 ))
    MEKHAN_MAILPIT_UI_PORT=$(( base + 31 ))
    MEKHAN_HTTPBIN_PORT=$(( base + 32 ))
    MEKHAN_LIVEKIT_PORT=$(( base + 40 ))
    MEKHAN_LIVEKIT_RTC_TCP_PORT=$(( base + 41 ))
    MEKHAN_LIVEKIT_RTC_UDP_PORT=$(( base + 42 ))
    # Private compose project → containers, networks, AND named volumes are all
    # prefixed `mekhan-s<slot>_…`, so `up`/`down`/`reset` only ever touch this
    # worktree's infra.
    COMPOSE_PROJECT_NAME="mekhan-s${slot}"
    export COMPOSE_PROJECT_NAME
fi

# ── Composite endpoints derived from the ports above. ────────────────────────
# These are what the daemons actually consume; deriving them here keeps the
# port→URL mapping in one place.
MEKHAN_SERVICE_URL="http://localhost:${MEKHAN_SERVICE_PORT}"
MEKHAN_ENGINE_URL="http://localhost:${MEKHAN_ENGINE_PORT}"
MEKHAN_NATS_URL="nats://localhost:${MEKHAN_NATS_PORT}"
MEKHAN_NATS_MON_URL="http://localhost:${MEKHAN_NATS_MON_PORT}"
MEKHAN_DATABASE_URL="postgres://mekhan:mekhan@localhost:${MEKHAN_PG_PORT}/mekhan"
MEKHAN_S3_ENDPOINT="http://localhost:${MEKHAN_S3_PORT}"
MEKHAN_VAULT_ADDR="http://localhost:${MEKHAN_VAULT_PORT}"
MEKHAN_LIVEKIT_URL="ws://localhost:${MEKHAN_LIVEKIT_PORT}"

export \
    WORKTREE_SLOT \
    MEKHAN_SERVICE_PORT MEKHAN_ENGINE_PORT MEKHAN_CANCEL_PORT MEKHAN_APP_PORT \
    MEKHAN_PG_PORT MEKHAN_NATS_PORT MEKHAN_NATS_MON_PORT MEKHAN_VAULT_PORT \
    MEKHAN_S3_PORT MEKHAN_S3_CONSOLE_PORT \
    MEKHAN_ZITADEL_DB_PORT MEKHAN_ZITADEL_PORT \
    MEKHAN_MAILPIT_SMTP_PORT MEKHAN_MAILPIT_UI_PORT MEKHAN_HTTPBIN_PORT \
    MEKHAN_LIVEKIT_PORT MEKHAN_LIVEKIT_RTC_TCP_PORT MEKHAN_LIVEKIT_RTC_UDP_PORT \
    MEKHAN_SERVICE_URL MEKHAN_ENGINE_URL MEKHAN_NATS_URL MEKHAN_NATS_MON_URL \
    MEKHAN_DATABASE_URL MEKHAN_S3_ENDPOINT MEKHAN_VAULT_ADDR MEKHAN_LIVEKIT_URL

if [[ "${1:-}" == "--print" ]]; then
    cat <<EOF
WORKTREE_SLOT=${WORKTREE_SLOT}
COMPOSE_PROJECT_NAME=${COMPOSE_PROJECT_NAME:-<unset: default project>}
── native daemons ──
MEKHAN_SERVICE_PORT=${MEKHAN_SERVICE_PORT}   (${MEKHAN_SERVICE_URL})
MEKHAN_ENGINE_PORT=${MEKHAN_ENGINE_PORT}    (${MEKHAN_ENGINE_URL})
MEKHAN_CANCEL_PORT=${MEKHAN_CANCEL_PORT}
MEKHAN_APP_PORT=${MEKHAN_APP_PORT}
── infra (compose) ──
MEKHAN_PG_PORT=${MEKHAN_PG_PORT}      (${MEKHAN_DATABASE_URL})
MEKHAN_NATS_PORT=${MEKHAN_NATS_PORT}    (${MEKHAN_NATS_URL})
MEKHAN_NATS_MON_PORT=${MEKHAN_NATS_MON_PORT}
MEKHAN_VAULT_PORT=${MEKHAN_VAULT_PORT}    (${MEKHAN_VAULT_ADDR})
MEKHAN_S3_PORT=${MEKHAN_S3_PORT}      (${MEKHAN_S3_ENDPOINT})
MEKHAN_S3_CONSOLE_PORT=${MEKHAN_S3_CONSOLE_PORT}
── optional ──
MEKHAN_ZITADEL_DB_PORT=${MEKHAN_ZITADEL_DB_PORT}
MEKHAN_ZITADEL_PORT=${MEKHAN_ZITADEL_PORT}
MEKHAN_MAILPIT_SMTP_PORT=${MEKHAN_MAILPIT_SMTP_PORT}
MEKHAN_MAILPIT_UI_PORT=${MEKHAN_MAILPIT_UI_PORT}
MEKHAN_HTTPBIN_PORT=${MEKHAN_HTTPBIN_PORT}
MEKHAN_LIVEKIT_PORT=${MEKHAN_LIVEKIT_PORT}    (${MEKHAN_LIVEKIT_URL})
MEKHAN_LIVEKIT_RTC_TCP_PORT=${MEKHAN_LIVEKIT_RTC_TCP_PORT}
MEKHAN_LIVEKIT_RTC_UDP_PORT=${MEKHAN_LIVEKIT_RTC_UDP_PORT}
EOF
fi
