#!/usr/bin/env bash
# Extract the xArm 6 robot description (URDF + referenced meshes) from a running
# xarm sim container into a bundle the platform serves as a `robot_description`
# asset (resolved by `robot_model` → loaded by the in-browser Threlte URDF twin).
#
# Source of truth = the live `/robot_state_publisher` `robot_description` param:
# the fully xacro-resolved URDF that MoveIt/RViz themselves consume. We then copy
# only the meshes that URDF references, preserving the `package://xarm_description/…`
# path so the browser URDF loader can resolve them from the unzipped archive.
#
# Output (committed, uploaded to the asset at seed time):
#   deploy/dev/xarm/robot_description_bundle/xarm6.urdf
#   deploy/dev/xarm/robot_description_bundle/xarm6_meshes.zip   (xarm_description/meshes/…)
#
# Usage: deploy/dev/xarm/extract-robot-description.sh [container]   (default: mekhan-s0-xarm)
set -euo pipefail
C="${1:-mekhan-s0-xarm}"
OUT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/robot_description_bundle"
mkdir -p "$OUT_DIR"

echo "→ grabbing resolved /robot_description from $C …"
docker exec "$C" bash -lc '
  source /opt/ros/jazzy/setup.bash; source /ros2_ws/install/setup.bash 2>/dev/null
  export ROS_DOMAIN_ID=42
  python3 - <<"PY"
import rclpy, re, os, shutil
from rclpy.node import Node
from rcl_interfaces.srv import GetParameters
rclpy.init(); n = Node("urdf_grab")
cli = n.create_client(GetParameters, "/robot_state_publisher/get_parameters")
assert cli.wait_for_service(timeout_sec=10), "robot_state_publisher param service absent"
req = GetParameters.Request(); req.names = ["robot_description"]
fut = cli.call_async(req); rclpy.spin_until_future_complete(n, fut, timeout_sec=10)
urdf = fut.result().values[0].string_value
open("/tmp/xarm6.urdf", "w").write(urdf)

share = "/ros2_ws/install/xarm_description/share/xarm_description"
B = "/tmp/xarm6_bundle"; shutil.rmtree(B, ignore_errors=True); os.makedirs(B)
refs = sorted(set(re.findall(r"package://([^\"\x27]+)", urdf)))
for r in refs:                                  # r = xarm_description/meshes/…
    rel = r.split("xarm_description/", 1)[1]     # meshes/…
    dst = os.path.join(B, r)                     # keep package path
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    shutil.copy(os.path.join(share, rel), dst)
print(f"urdf {len(urdf)}B, {len(refs)} meshes")
rclpy.shutdown()
PY
  cd /tmp/xarm6_bundle && zip -qr /tmp/xarm6_meshes.zip xarm_description
' 2>&1 | grep -v 'WARNING\|share an exact name' || true

docker cp "$C":/tmp/xarm6.urdf       "$OUT_DIR/xarm6.urdf"
docker cp "$C":/tmp/xarm6_meshes.zip  "$OUT_DIR/xarm6_meshes.zip"
echo "✓ bundle → $OUT_DIR"
ls -la "$OUT_DIR"
