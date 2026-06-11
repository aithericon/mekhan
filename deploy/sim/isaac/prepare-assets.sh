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
# Lock the gripper's five mimic finger joints to FIXED. PhysX's mimic
# constraints fight the joint limits (fingers observed 48° beyond their hard
# stop) and the solver impulses yank the wrist — joint6 deflected 0.18+ rad
# mid-trajectory, aborting MoveIt executions. Grasping on this stack is MoveIt
# scene-attach, not finger physics; drive_joint stays revolute so the gripper
# traj controller keeps its actuated joint + state feedback.
def lock_mimics(m):
    j = m.group(0)
    # drive_joint too: even with the five mimics fixed, a live drive_joint
    # DOF deterministically destabilized the wrist on all-zeros goals (j6 and
    # drive_joint flung in coupled, limit-violating excursions). The gripper
    # action is best-effort in motion_bridge and grasping is scene-attach, so
    # Isaac needs no gripper DOF at all.
    if "<mimic" not in j and 'name="drive_joint"' not in j:
        return j
    j = re.sub(r'type="revolute"', 'type="fixed"', j, count=1)
    j = re.sub(r"<mimic [^/]*/>", "", j)
    j = re.sub(r"<limit [^/]*/>", "", j)
    j = re.sub(r"<axis [^/]*/>", "", j)
    return j
src, n = re.subn(r"<joint\b.*?</joint>", lock_mimics, src, flags=re.S)
locked = len(re.findall(r'type="fixed"', src))
print(f"checked {n} joints; {locked} fixed joints after mimic lock")
out = os.path.join(os.environ["OUT"], "xarm6_isaac.urdf")
open(out, "w").write(src)
meshes = len(re.findall(r'filename="xarm_description/', src))
print(f"✓ {out} ({len(src)}B, {meshes} relative mesh refs)")
PY

echo "✓ assets ready: $OUT"
