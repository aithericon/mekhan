#!/usr/bin/env bash
# Prepare the xArm6 URDF + meshes for Isaac Sim's URDF importer from the
# committed robot-description asset (demos/assets/files/xarm6.urdf — the fully
# xacro-resolved URDF the browser twin already consumes).
#
# Two transforms, into ./assets/:
#   1. strip the <ros2_control> blocks (they reference the uf_robot_hardware /
#      mock plugins; the hardware seam in the Isaac stack lives in the xarm
#      CONTAINER's topic_based_ros2_control, not in the URDF Isaac loads)
#   2. rewrite package://xarm_description/… mesh refs to paths relative to the
#      URDF (the importer has no ROS package index), and unzip the mesh bundle
#      into that layout.
#
# Idempotent. Needs python3 + unzip. Run on the deploy host before first
# `docker compose up` (sync-to-host.sh does this automatically).
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# In-repo layout: repo_root/deploy/sim/isaac → assets live at repo_root/demos/.
# Synced layout (sync-to-host.sh): the bundle is dropped into ./bundle/.
if [ -f "$HERE/bundle/xarm6.urdf" ]; then
  SRC_URDF="$HERE/bundle/xarm6.urdf"; SRC_ZIP="$HERE/bundle/xarm6_meshes.zip"
else
  ROOT="$(cd "$HERE/../../.." && pwd)"
  SRC_URDF="$ROOT/demos/assets/files/xarm6.urdf"; SRC_ZIP="$ROOT/demos/assets/files/xarm6_meshes.zip"
fi
[ -f "$SRC_URDF" ] || { echo "missing $SRC_URDF" >&2; exit 1; }

OUT="$HERE/assets"
mkdir -p "$OUT"
rm -rf "$OUT/xarm_description"
unzip -qo "$SRC_ZIP" -d "$OUT"   # → assets/xarm_description/meshes/…

SRC_URDF="$SRC_URDF" OUT="$OUT" python3 - <<'PY'
import os, re

src = open(os.environ["SRC_URDF"]).read()
# Drop every <ros2_control>…</ros2_control> system block (single-line safe).
src = re.sub(r"<ros2_control\b.*?</ros2_control>", "", src, flags=re.S)
assert "<ros2_control" not in src
# Mesh refs: package://xarm_description/… → relative to the URDF file.
src = src.replace("package://xarm_description/", "xarm_description/")
out = os.path.join(os.environ["OUT"], "xarm6_isaac.urdf")
open(out, "w").write(src)
meshes = len(re.findall(r'filename="xarm_description/', src))
print(f"✓ {out} ({len(src)}B, {meshes} relative mesh refs)")
PY

echo "✓ assets ready: $OUT"
