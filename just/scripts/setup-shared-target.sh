#!/usr/bin/env bash
# Set up shared cargo target dirs for this worktree.
#
# This monorepo has three Cargo workspaces (umbrella `./`, `engine/`,
# `executor/`). Without sharing, each worktree builds its own ~80–230 GB of
# artifacts. This script writes per-workspace `.envrc` files that export
# `CARGO_TARGET_DIR` to a single shared location, so every worktree on this
# machine reuses the same compiled deps.
#
# Trade-off: cargo serializes builds against one target dir via its
# `.rustc_info.json` lock — two simultaneous `cargo build` invocations in
# different worktrees will wait on each other. Editing / running / testing
# in parallel is unaffected; only concurrent compiles serialize.
#
# Usage:
#   just/scripts/setup-shared-target.sh          # write .envrc files + direnv allow
#   just/scripts/setup-shared-target.sh --migrate # additionally mv existing target/ → shared
#   just/scripts/setup-shared-target.sh --clean   # additionally rm existing target/ in this worktree
#
# Env overrides:
#   SHARED_CARGO_TARGET_BASE  default: $HOME/.cache/cargo-targets/aithericon-platform
#
# Idempotent — re-run in any worktree to opt that worktree in.

set -euo pipefail

MIGRATE=0
CLEAN=0
for arg in "$@"; do
    case "$arg" in
        --migrate) MIGRATE=1 ;;
        --clean)   CLEAN=1 ;;
        -h|--help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "unknown flag: $arg" >&2; exit 2 ;;
    esac
done

if [[ $MIGRATE -eq 1 && $CLEAN -eq 1 ]]; then
    echo "--migrate and --clean are mutually exclusive" >&2
    exit 2
fi

if ! command -v direnv >/dev/null 2>&1; then
    echo "direnv not found. Install with: brew install direnv" >&2
    echo "Then add 'eval \"\$(direnv hook bash)\"' (or zsh) to your shell rc." >&2
    exit 1
fi

ROOT="$(git rev-parse --show-toplevel)"
SHARED_BASE="${SHARED_CARGO_TARGET_BASE:-$HOME/.cache/cargo-targets/aithericon-platform}"
# What gets written into the .envrc file. Default uses literal "$HOME" so the
# file is portable across users / machines (direnv evaluates as bash). If
# SHARED_CARGO_TARGET_BASE is set to a non-default value, we write the
# resolved absolute path instead — the override is the user's explicit intent.
if [[ -n "${SHARED_CARGO_TARGET_BASE-}" && "$SHARED_CARGO_TARGET_BASE" != "$HOME/.cache/cargo-targets/aithericon-platform" ]]; then
    SHARED_BASE_LITERAL="$SHARED_BASE"
else
    SHARED_BASE_LITERAL='$HOME/.cache/cargo-targets/aithericon-platform'
fi

# workspace_dir : workspace_name (used as subdir in $SHARED_BASE)
WORKSPACES=(
    "$ROOT:umbrella"
    "$ROOT/engine:engine"
    "$ROOT/executor:executor"
)

echo "Shared target base: $SHARED_BASE"
echo "Worktree:           $ROOT"
echo

MARKER_START="# >>> shared-cargo-target (managed by setup-shared-target.sh) >>>"
MARKER_END="# <<< shared-cargo-target <<<"

for entry in "${WORKSPACES[@]}"; do
    ws_dir="${entry%%:*}"
    ws_name="${entry##*:}"
    shared_dir="$SHARED_BASE/$ws_name"
    shared_dir_literal="$SHARED_BASE_LITERAL/$ws_name"
    envrc="$ws_dir/.envrc"
    existing_target="$ws_dir/target"

    mkdir -p "$shared_dir"

    # Manage a marker-delimited block inside .envrc so we never clobber
    # adjacent direnv directives (umbrella's `use flake`, hypothetical
    # per-workspace tooling, etc.). Block contents use `$HOME` literally so
    # the file is portable.
    block=$(cat <<EOF
$MARKER_START
# Shares the cargo target dir across all worktrees of this workspace.
# Delete this block and run 'direnv reload' to opt out.
export CARGO_TARGET_DIR="$shared_dir_literal"
$MARKER_END
EOF
)

    if [[ -f "$envrc" ]] && grep -qF "$MARKER_START" "$envrc"; then
        # Replace the existing marker block in place. BSD awk on macOS
        # rejects multi-line strings in `-v` assignments, so we splice the
        # replacement block from a temp file via `getline`.
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
            echo "  updated  $envrc (refreshed marker block)"
        fi
    elif [[ -f "$envrc" ]]; then
        # File exists but no marker block — append, preserving original
        # content (e.g. `use flake`).
        [[ -s "$envrc" && -n "$(tail -c1 "$envrc")" ]] && echo >> "$envrc"
        printf '%s\n' "$block" >> "$envrc"
        echo "  appended block to $envrc"
    else
        printf '%s\n' "$block" > "$envrc"
        echo "  wrote    $envrc"
    fi

    (cd "$ws_dir" && direnv allow . >/dev/null)

    # Handle the pre-existing target/ directory at this workspace root.
    if [[ -L "$existing_target" ]]; then
        : # already a symlink, leave alone
    elif [[ -d "$existing_target" ]]; then
        if [[ $MIGRATE -eq 1 ]]; then
            # Move contents into the shared dir, preserving existing build cache.
            # Use mv to keep this fast (same-filesystem rename). On collision,
            # the existing shared content wins — anything newer in the worktree
            # will be rebuilt on next cargo run.
            shopt -s dotglob nullglob
            files=("$existing_target"/*)
            shopt -u dotglob nullglob
            if [[ ${#files[@]} -gt 0 ]]; then
                echo "  migrating $existing_target → $shared_dir ..."
                for f in "${files[@]}"; do
                    name="$(basename "$f")"
                    if [[ -e "$shared_dir/$name" ]]; then
                        echo "    skip (already in shared): $name"
                        rm -rf "$f"
                    else
                        mv "$f" "$shared_dir/"
                    fi
                done
            fi
            rmdir "$existing_target" 2>/dev/null || rm -rf "$existing_target"
            echo "  removed $existing_target"
        elif [[ $CLEAN -eq 1 ]]; then
            echo "  removing $existing_target (--clean) ..."
            rm -rf "$existing_target"
        else
            echo "  NOTE: $existing_target still exists. Re-run with --migrate to preserve it, --clean to delete it."
        fi
    fi

    echo "  → $ws_name: $shared_dir ($(du -sh "$shared_dir" 2>/dev/null | cut -f1))"
    echo
done

echo "Done. Verify with: cargo metadata --format-version=1 | jq -r '.target_directory'"
