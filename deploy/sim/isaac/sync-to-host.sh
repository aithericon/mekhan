#!/usr/bin/env bash
# Sync the Isaac Sim xArm stack to a GPU deploy host and prepare its assets.
# The host needs: docker + compose v2, nvidia-container-toolkit, an RTX-class
# GPU, python3 + unzip + rsync (hydra-2 qualifies — probed 2026-06-11).
#
# Reproduces the repo-relative layout under $DEST so docker-compose.yml's
# `build: ../../dev/xarm` context resolves identically on the host:
#   $DEST/deploy/sim/isaac/      this directory (compose, scripts, profiles)
#   $DEST/deploy/dev/xarm/       the xarm image build context
#   $DEST/deploy/sim/isaac/bundle/  the committed URDF+mesh asset bundle
#
# Usage: deploy/sim/isaac/sync-to-host.sh [user@host] [dest-dir]
#        then: ssh user@host 'cd <dest>/deploy/sim/isaac && docker compose up -d --build'
set -euo pipefail
HOST="${1:-hydra-2@131.246.221.73}"
DEST="${2:-aithericon-sim}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"

echo "→ syncing to $HOST:$DEST …"
ssh "$HOST" "mkdir -p $DEST/deploy/sim/isaac/bundle $DEST/deploy/dev/xarm"
rsync -az --delete --exclude assets --exclude bundle "$HERE/" "$HOST:$DEST/deploy/sim/isaac/"
rsync -az --delete "$ROOT/deploy/dev/xarm/" "$HOST:$DEST/deploy/dev/xarm/"
rsync -az "$ROOT/demos/assets/files/xarm6.urdf" "$ROOT/demos/assets/files/xarm6_meshes.zip" \
      "$HOST:$DEST/deploy/sim/isaac/bundle/"

echo "→ preparing Isaac URDF assets on $HOST …"
ssh "$HOST" "cd $DEST/deploy/sim/isaac && bash prepare-assets.sh"

echo "✓ synced. Next:"
echo "    ssh $HOST 'cd $DEST/deploy/sim/isaac && docker compose up -d --build'"
