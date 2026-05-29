#!/usr/bin/env bash
# Set up sccache as cargo's rustc wrapper across this repo's three workspaces.
#
# Replaces the older shared-CARGO_TARGET_DIR scheme. The trade-off:
#   - shared CARGO_TARGET_DIR  → small disk (~80 GB total), but cargo's
#                                 per-target-dir lock serializes builds across
#                                 worktrees, killing parallel work
#   - sccache                  → larger disk (~80 GB per worktree + ~30 GB
#                                 sccache cache), but worktrees have their own
#                                 target/ so concurrent `cargo build` no longer
#                                 blocks, and cold/post-`cargo clean` rebuilds
#                                 hit the content-addressed sccache cache and
#                                 finish dramatically faster
#
# What this writes into each workspace's `.envrc`:
#     export RUSTC_WRAPPER="sccache"
#     export SCCACHE_CACHE_SIZE="30G"
#     export CARGO_INCREMENTAL="0"   # required: sccache won't cache incremental
#
# Usage:
#   just/scripts/setup-cargo-cache.sh                # write .envrc blocks + direnv allow
#   just/scripts/setup-cargo-cache.sh --clean-shared # additionally delete the legacy
#                                                    # ~/.cache/cargo-targets/aithericon-platform/
#                                                    # shared dirs (recovers disk)
#
# Env overrides:
#   SCCACHE_CACHE_SIZE_OVERRIDE  default: 30G
#
# Idempotent — re-run in any worktree to opt that worktree in / refresh blocks.

set -euo pipefail

CLEAN_SHARED=0
SLOT_ARG=""   # per-worktree dev-stack slot; empty = "keep existing / default 0"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --clean-shared) CLEAN_SHARED=1 ;;
        --slot) SLOT_ARG="${2:-}"; shift ;;
        --slot=*) SLOT_ARG="${1#--slot=}" ;;
        -h|--help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
    shift
done
if [[ -n "$SLOT_ARG" ]] && ! [[ "$SLOT_ARG" =~ ^[0-9]+$ ]]; then
    echo "--slot must be a non-negative integer, got '$SLOT_ARG'" >&2
    exit 2
fi

if ! command -v direnv >/dev/null 2>&1; then
    echo "direnv not found. Install with: brew install direnv" >&2
    echo "Then add 'eval \"\$(direnv hook bash)\"' (or zsh) to your shell rc." >&2
    exit 1
fi

if ! command -v sccache >/dev/null 2>&1; then
    echo "sccache not found. Install with: brew install sccache" >&2
    exit 1
fi

if ! command -v cargo-sweep >/dev/null 2>&1; then
    echo "cargo-sweep not found (used by 'just dev gc-targets'). Install with: brew install cargo-sweep" >&2
    # Not fatal — sccache itself is what makes the build cache work; cargo-sweep
    # is only needed for the periodic target/ GC recipe.
fi

ROOT="$(git rev-parse --show-toplevel)"
CACHE_SIZE="${SCCACHE_CACHE_SIZE_OVERRIDE:-100G}"

WORKSPACES=(
    "$ROOT:umbrella"
    "$ROOT/engine:engine"
    "$ROOT/executor:executor"
)

echo "sccache version:    $(sccache --version)"
echo "Cache size limit:   $CACHE_SIZE"
echo "Worktree:           $ROOT"
echo

MARKER_START="# >>> cargo-sccache (managed by setup-cargo-cache.sh) >>>"
MARKER_END="# <<< cargo-sccache <<<"
LEGACY_MARKER_START="# >>> shared-cargo-target (managed by setup-shared-target.sh) >>>"
LEGACY_MARKER_END="# <<< shared-cargo-target <<<"

for entry in "${WORKSPACES[@]}"; do
    ws_dir="${entry%%:*}"
    ws_name="${entry##*:}"
    envrc="$ws_dir/.envrc"

    block=$(cat <<EOF
$MARKER_START
# Caches compiled crates in ~/Library/Caches/Mozilla.sccache so worktrees
# don't recompile the same deps. Per-worktree target/ dirs are preserved
# (cargo's default), so concurrent \`cargo build\` in different worktrees
# no longer contends on the cargo target-dir lock. CARGO_INCREMENTAL=0 is
# required for sccache to cache dev builds — sccache refuses to cache
# incremental-mode artifacts.
# Delete this block and run 'direnv reload' to opt out.
export RUSTC_WRAPPER="sccache"
export SCCACHE_CACHE_SIZE="$CACHE_SIZE"
export CARGO_INCREMENTAL="0"
$MARKER_END
EOF
)

    # If a legacy shared-target block exists, strip it before writing the new
    # one. Two passes is simpler than splicing two different marker pairs.
    if [[ -f "$envrc" ]] && grep -qF "$LEGACY_MARKER_START" "$envrc"; then
        awk -v start="$LEGACY_MARKER_START" -v end="$LEGACY_MARKER_END" '
            $0 == start { in_block = 1; next }
            in_block && $0 == end { in_block = 0; next }
            !in_block { print }
        ' "$envrc" > "$envrc.tmp"
        mv "$envrc.tmp" "$envrc"
        echo "  cleaned legacy shared-target block from $envrc"
    fi

    if [[ -f "$envrc" ]] && grep -qF "$MARKER_START" "$envrc"; then
        # Replace the existing sccache marker block in place. BSD awk on
        # macOS rejects multi-line strings in `-v` assignments, so splice
        # the replacement block from a temp file via `getline`.
        blockfile=$(mktemp)
        printf '%s\n' "$block" > "$blockfile"
        awk -v start="$MARKER_START" -v end="$MARKER_END" -v blockfile="$blockfile" '
            $0 == start {
                while ((getline line < blockfile) > 0) print line
                close(blockfile)
                in_block = 1
                next
            }
            in_block && $0 == end { in_block = 0; next }
            !in_block { print }
        ' "$envrc" > "$envrc.tmp"
        rm -f "$blockfile"
        if diff -q "$envrc" "$envrc.tmp" >/dev/null 2>&1; then
            rm "$envrc.tmp"
            echo "  unchanged $envrc"
        else
            mv "$envrc.tmp" "$envrc"
            echo "  updated  $envrc (refreshed sccache block)"
        fi
    elif [[ -f "$envrc" ]]; then
        # File exists but no sccache block — append, preserving original
        # content (e.g. `use flake`).
        [[ -s "$envrc" && -n "$(tail -c1 "$envrc")" ]] && echo >> "$envrc"
        printf '%s\n' "$block" >> "$envrc"
        echo "  appended block to $envrc"
    else
        printf '%s\n' "$block" > "$envrc"
        echo "  wrote    $envrc"
    fi

    (cd "$ws_dir" && direnv allow . >/dev/null)
    echo "  → $ws_name: sccache wired"
    echo
done

# ── Per-worktree dev-stack slot (umbrella .envrc only) ───────────────────────
# Writes a managed block to the umbrella .envrc that reads WORKTREE_SLOT from
# the gitignored .dev/slot file and sources dev-ports.sh — so a direnv-hooked
# shell in this worktree gets a private port block + compose project.
#
# The .envrc block is IDENTICAL across worktrees (so it's safe to commit —
# .envrc is tracked); the per-worktree number lives in .dev/slot (.dev/ is
# gitignored). Slot resolution: explicit --slot, else the slot already in this
# worktree's .dev/slot (idempotent re-runs keep it), else 0 (main checkout —
# .dev/slot is left absent so it stays on the legacy fixed ports).
DEVENV_MARKER_START="# >>> mekhan-dev-env (managed by setup-cargo-cache.sh) >>>"
DEVENV_MARKER_END="# <<< mekhan-dev-env <<<"
umbrella_envrc="$ROOT/.envrc"
slot_file="$ROOT/.dev/slot"

if [[ -n "$SLOT_ARG" ]]; then
    SLOT="$SLOT_ARG"
elif [[ -f "$slot_file" ]] && grep -qE '^[0-9]+$' "$slot_file"; then
    SLOT="$(grep -oE '^[0-9]+$' "$slot_file" | head -1)"
else
    SLOT=0
fi

# Persist the slot to .dev/slot when it's non-default or explicitly chosen, so
# the .envrc expression picks it up. A plain slot-0 main checkout gets no file.
if [[ -n "$SLOT_ARG" || "$SLOT" -ne 0 || -f "$slot_file" ]]; then
    mkdir -p "$ROOT/.dev"
    printf '%s\n' "$SLOT" > "$slot_file"
fi

devenv_block=$(cat <<'EOF'
# >>> mekhan-dev-env (managed by setup-cargo-cache.sh) >>>
# Per-worktree dev-stack isolation: WORKTREE_SLOT selects a private host-port
# block + docker compose project so `just dev` in this worktree never collides
# with another. The number lives in the gitignored .dev/slot (absent → slot 0,
# the main checkout's legacy fixed ports); slot N = a 100-wide block at
# 20000+N*100. See just/scripts/dev-ports.sh. Rebase this worktree's slot with:
#   just dev::setup-cargo-cache --slot N   (then `direnv reload`)
export WORKTREE_SLOT="$(cat .dev/slot 2>/dev/null || echo 0)"
source ./just/scripts/dev-ports.sh
# <<< mekhan-dev-env <<<
EOF
)

if [[ -f "$umbrella_envrc" ]] && grep -qF "$DEVENV_MARKER_START" "$umbrella_envrc"; then
    blockfile=$(mktemp)
    printf '%s\n' "$devenv_block" > "$blockfile"
    awk -v start="$DEVENV_MARKER_START" -v end="$DEVENV_MARKER_END" -v blockfile="$blockfile" '
        $0 == start {
            while ((getline line < blockfile) > 0) print line
            close(blockfile)
            in_block = 1
            next
        }
        in_block && $0 == end { in_block = 0; next }
        !in_block { print }
    ' "$umbrella_envrc" > "$umbrella_envrc.tmp"
    rm -f "$blockfile"
    if diff -q "$umbrella_envrc" "$umbrella_envrc.tmp" >/dev/null 2>&1; then
        rm "$umbrella_envrc.tmp"
        echo "  dev-env: slot $SLOT (unchanged)"
    else
        mv "$umbrella_envrc.tmp" "$umbrella_envrc"
        echo "  dev-env: slot $SLOT (refreshed $umbrella_envrc)"
    fi
elif [[ -f "$umbrella_envrc" ]]; then
    [[ -s "$umbrella_envrc" && -n "$(tail -c1 "$umbrella_envrc")" ]] && echo >> "$umbrella_envrc"
    printf '%s\n' "$devenv_block" >> "$umbrella_envrc"
    echo "  dev-env: slot $SLOT (appended to $umbrella_envrc)"
else
    printf '%s\n' "$devenv_block" > "$umbrella_envrc"
    echo "  dev-env: slot $SLOT (wrote $umbrella_envrc)"
fi
# Re-allow so direnv picks up the new/changed block immediately.
(cd "$ROOT" && direnv allow . >/dev/null)
echo

legacy_base="$HOME/.cache/cargo-targets/aithericon-platform"
if [[ $CLEAN_SHARED -eq 1 ]]; then
    if [[ -d "$legacy_base" ]]; then
        size="$(du -sh "$legacy_base" 2>/dev/null | cut -f1)"
        echo "▶ removing legacy shared target dirs at $legacy_base ($size)…"
        rm -rf "$legacy_base"
        echo "  ✓ removed"
    else
        echo "  · no legacy shared target dirs at $legacy_base"
    fi
elif [[ -d "$legacy_base" ]]; then
    size="$(du -sh "$legacy_base" 2>/dev/null | cut -f1)"
    echo "  NOTE: legacy shared target dirs still occupy $size at $legacy_base"
    echo "        re-run with --clean-shared to delete them."
fi

echo
echo "Done. Start a fresh shell (or 'direnv reload') so the new env takes effect."
echo "Verify with:   sccache --show-stats"
