#!/usr/bin/env python3
"""Headless Isaac Sim scene hosting the xArm6 as the physics "hardware" behind
the platform's xarm MoveIt container (deploy/dev/xarm, HW_BACKEND=isaac).

Contract (NVIDIA's canonical MoveIt <-> Isaac seam, DDS domain 42):
  subscribe  /isaac_joint_commands  sensor_msgs/JointState   <- topic_based_ros2_control
  publish    /isaac_joint_states    sensor_msgs/JointState   -> topic_based_ros2_control

The xarm container's two TopicBasedSystem hardware instances (arm + gripper)
publish position targets onto /isaac_joint_commands; the articulation
controller here applies them as PD position targets, PhysX integrates, and the
resulting joint states stream back on /isaac_joint_states — closing the
ros2_control loop through real physics. MoveIt, motion_bridge, rosbridge and
the executor ROS backend are all upstream of that loop and run unchanged.

Run inside nvcr.io/nvidia/isaac-sim:5.1.0:  ./python.sh isaac_xarm_scene.py
Env knobs:
  XARM_URDF        path to the prepared URDF (default /assets/xarm6_isaac.urdf,
                   produced by prepare-assets.sh: ros2_control blocks stripped,
                   package:// mesh refs relativized)
  ISAAC_HEADLESS   "1" (default) headless / "0" with viewport (local debug)
  SIM_HZ           physics rate (default 60)
"""

import os
import sys

from isaacsim import SimulationApp

HEADLESS = os.environ.get("ISAAC_HEADLESS", "1") != "0"
SIM_HZ = float(os.environ.get("SIM_HZ", "60"))

simulation_app = SimulationApp({"headless": HEADLESS})

# Kit is alive — omni.* / isaacsim.* / pxr imports are legal only from here on.
import omni.graph.core as og  # noqa: E402
import omni.kit.commands  # noqa: E402
import omni.timeline  # noqa: E402
from isaacsim.core.api import World  # noqa: E402
from isaacsim.core.utils.extensions import enable_extension  # noqa: E402

# The ROS 2 bridge picks up ROS_DOMAIN_ID / RMW_IMPLEMENTATION /
# FASTRTPS_DEFAULT_PROFILES_FILE from the environment (compose sets them) and
# auto-loads Isaac's internal ROS 2 Jazzy libs since nothing else is sourced.
enable_extension("isaacsim.ros2.bridge")
simulation_app.update()

URDF_PATH = os.environ.get("XARM_URDF", "/assets/xarm6_isaac.urdf")
if not os.path.exists(URDF_PATH):
    print(f"[isaac_xarm_scene] URDF not found: {URDF_PATH} — run prepare-assets.sh first", file=sys.stderr)
    simulation_app.close()
    sys.exit(2)


def import_xarm(urdf_path: str) -> str:
    """Import the xArm6 URDF as a fixed-base articulation; return its prim path."""
    from isaacsim.asset.importer.urdf import _urdf

    cfg = _urdf.ImportConfig()
    cfg.fix_base = True
    cfg.merge_fixed_joints = False
    cfg.convex_decomp = False
    cfg.self_collision = False
    cfg.make_default_prim = True
    cfg.create_physics_scene = True
    cfg.distance_scale = 1.0
    # PD gains for the position drives (NVIDIA's MoveIt-tutorial values; the
    # xarm URDF carries no <dynamics> so the importer needs explicit defaults).
    cfg.default_drive_type = _urdf.UrdfJointTargetType.JOINT_DRIVE_POSITION
    cfg.default_drive_strength = 1047.19751
    cfg.default_position_drive_damping = 52.35988

    status, prim_path = omni.kit.commands.execute(
        "URDFParseAndImportFile", urdf_path=urdf_path, import_config=cfg
    )
    if not status:
        raise RuntimeError(f"URDF import failed: {urdf_path}")
    print(f"[isaac_xarm_scene] imported {urdf_path} -> {prim_path}")
    return prim_path


robot_prim = import_xarm(URDF_PATH)


def find_articulation_root(base_path: str) -> str:
    """The og articulation/joint-state nodes need the prim carrying
    UsdPhysics.ArticulationRootAPI — the importer applies it to a DESCENDANT
    (base link / root joint), not the robot's top-level prim; targeting the
    top level yields 'Failed to find articulation' spam from the tensors
    plugin."""
    import omni.usd
    from pxr import Usd, UsdPhysics

    stage = omni.usd.get_context().get_stage()
    for prim in Usd.PrimRange(stage.GetPrimAtPath(base_path)):
        if prim.HasAPI(UsdPhysics.ArticulationRootAPI):
            return prim.GetPath().pathString
    print(f"[isaac_xarm_scene] WARN: no ArticulationRootAPI under {base_path}, using it as-is")
    return base_path


art_root = find_articulation_root(robot_prim)
print(f"[isaac_xarm_scene] articulation root: {art_root}")

world = World(stage_units_in_meters=1.0, physics_dt=1.0 / SIM_HZ, rendering_dt=1.0 / SIM_HZ)
world.scene.add_default_ground_plane()

# ── ROS 2 bridge graph: tick → (subscribe commands → articulation controller,
#    publish joint states). Plain JointState both ways; TopicBasedSystem and
#    the Isaac nodes match joints BY NAME, so arm and gripper systems can share
#    the one topic pair.
keys = og.Controller.Keys
og.Controller.edit(
    {"graph_path": "/ActionGraph", "evaluator_name": "execution"},
    {
        keys.CREATE_NODES: [
            ("OnPlaybackTick", "omni.graph.action.OnPlaybackTick"),
            ("ReadSimTime", "isaacsim.core.nodes.IsaacReadSimulationTime"),
            ("Context", "isaacsim.ros2.bridge.ROS2Context"),
            ("SubscribeJointState", "isaacsim.ros2.bridge.ROS2SubscribeJointState"),
            ("ArticulationController", "isaacsim.core.nodes.IsaacArticulationController"),
            ("PublishJointState", "isaacsim.ros2.bridge.ROS2PublishJointState"),
        ],
        keys.CONNECT: [
            ("OnPlaybackTick.outputs:tick", "SubscribeJointState.inputs:execIn"),
            ("OnPlaybackTick.outputs:tick", "PublishJointState.inputs:execIn"),
            ("OnPlaybackTick.outputs:tick", "ArticulationController.inputs:execIn"),
            ("Context.outputs:context", "SubscribeJointState.inputs:context"),
            ("Context.outputs:context", "PublishJointState.inputs:context"),
            ("ReadSimTime.outputs:simulationTime", "PublishJointState.inputs:timeStamp"),
            ("SubscribeJointState.outputs:jointNames", "ArticulationController.inputs:jointNames"),
            ("SubscribeJointState.outputs:positionCommand", "ArticulationController.inputs:positionCommand"),
            ("SubscribeJointState.outputs:velocityCommand", "ArticulationController.inputs:velocityCommand"),
            ("SubscribeJointState.outputs:effortCommand", "ArticulationController.inputs:effortCommand"),
        ],
        keys.SET_VALUES: [
            ("Context.inputs:domain_id", int(os.environ.get("ROS_DOMAIN_ID", "42"))),
            ("SubscribeJointState.inputs:topicName", "/isaac_joint_commands"),
            ("PublishJointState.inputs:topicName", "/isaac_joint_states"),
            ("ArticulationController.inputs:targetPrim", [art_root]),
            ("PublishJointState.inputs:targetPrim", [art_root]),
        ],
    },
)

world.reset()
omni.timeline.get_timeline_interface().play()
print(f"[isaac_xarm_scene] running: {SIM_HZ:.0f} Hz physics, domain {os.environ.get('ROS_DOMAIN_ID', '42')}, "
      f"sub /isaac_joint_commands pub /isaac_joint_states")

# render=True even headless: app updates drive the OmniGraph playback tick (a
# physics-only step would never evaluate the ROS bridge nodes).
while simulation_app.is_running():
    world.step(render=True)

simulation_app.close()
