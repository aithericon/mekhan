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
Phase 1 scene mirror: motion_bridge publishes a declarative FULL SNAPSHOT of
the move_group world objects on /aithericon/scene_objects (std_msgs/String
JSON, see MIRROR_TOPIC in motion_bridge_node.py). A generic ROS2Subscriber og
node receives it and the main loop reconciles USD prims under
/World/AithericonMirror against the latest snapshot — so the work surface and
sample boxes EXIST sim-side. Attached (grasped) objects are withdrawn from the
world set while held (the MoveIt twin renders them riding the gripper).

Env knobs:
  XARM_URDF        path to the prepared URDF (default /assets/xarm6_isaac.urdf,
                   produced by prepare-assets.sh: ros2_control blocks stripped,
                   package:// mesh refs relativized)
  ISAAC_HEADLESS   "1" (default) headless / "0" with viewport (local debug)
  SIM_HZ           physics rate (default 60)
  MIRROR_COLLIDERS "0" (default) mirrored prims are VISUAL-ONLY. "1" applies
                   static colliders. Default off on purpose: the gripper is
                   scene-attach (no contact grasping yet), so a pick pose puts
                   locked gripper geometry INSIDE the sample box — a collider
                   there is exactly the phantom-contact constraint that broke
                   home approaches with the ground plane (P0 root cause).
"""

import json
import os
import re
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
    # convex_decomp=False would wrap each collision mesh in ONE convex hull —
    # the xarm's L-shaped links get their concavities filled, and near the
    # vertical (all-zeros) pose the wrist assembly sits inside a filled
    # concavity of a non-adjacent link. The resulting phantom contact is a
    # position-level constraint that shoves j4/j5/j6 ~0.1-0.2 rad off target
    # REGARDLESS of drive stiffness (the giveaway), failing every home-pose
    # trajectory while offset poses track exactly.
    cfg.convex_decomp = True
    cfg.self_collision = False
    cfg.make_default_prim = True
    cfg.create_physics_scene = True
    cfg.distance_scale = 1.0
    # PD gains for the position drives (xarm URDF carries no <dynamics>, so
    # the importer needs explicit defaults). These are NVIDIA's tutorial
    # values — proven stable here INCLUDING the gripper's five mimic joints;
    # a blanket 1e7/1e5 crashed PhysX mid-motion (mimic constraints fighting
    # mega-stiff drives). The six ARM joints get stiffened post-import
    # (stiffen_arm_drives) because at these defaults gravity sag left joint2
    # 0.015 rad short of goal (tolerance 0.01) → GOAL_TOLERANCE_VIOLATED.
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


def never_sleep(path: str) -> None:
    """Disable PhysX articulation sleeping. The vertical all-zeros pose is a
    zero-gravity-torque equilibrium: during a home move's deceleration tail
    the articulation's energy drops below the sleep threshold ~0.1 rad SHORT
    of target and PhysX puts it to sleep — and the OmniGraph articulation
    controller's tensor-API writes don't wake it, so it ignores all further
    commands (deterministic 'parked' pose, trajectory aborts, frozen arm).
    Offset goals never slept only because gravity torque keeps the drives
    correcting."""
    import omni.usd
    from pxr import PhysxSchema

    stage = omni.usd.get_context().get_stage()
    api = PhysxSchema.PhysxArticulationAPI.Apply(stage.GetPrimAtPath(path))
    api.CreateSleepThresholdAttr().Set(0.0)
    api.CreateStabilizationThresholdAttr().Set(0.0)
    print(f"[isaac_xarm_scene] sleep disabled on {path}")


never_sleep(art_root)


def scale_drives(base_path: str, joints: set, k_scale: float, f_scale: float) -> None:
    """Scale position-drive stiffness + max force on selected joints, RELATIVE
    to what the importer wrote (robust to USD's angular-drive unit
    convention), and set damping ABSOLUTELY to k/20 — the importer writes
    near-zero damping (1.0 arm / 0.0 gripper, ignoring the config default),
    which at high stiffness rings hard enough to blow the trajectory
    controller's 0.01 rad state tolerance mid-motion."""
    import omni.usd
    from pxr import Usd, UsdPhysics

    stage = omni.usd.get_context().get_stage()
    for prim in Usd.PrimRange(stage.GetPrimAtPath(base_path)):
        if prim.GetName() not in joints:
            continue
        drive = UsdPhysics.DriveAPI.Get(prim, "angular")
        if not drive:
            continue
        k = drive.GetStiffnessAttr().Get()
        mf = drive.GetMaxForceAttr().Get()
        new_k = k * k_scale
        new_d = new_k / 20.0
        drive.GetStiffnessAttr().Set(new_k)
        drive.GetDampingAttr().Set(new_d)
        if mf:  # 0/inf sentinel stays as-is
            drive.GetMaxForceAttr().Set(mf * f_scale)
        print(f"[isaac_xarm_scene] drive {prim.GetName()}: k {k:.1f}->{new_k:.1f} "
              f"d ->{new_d:.1f} maxF {mf}->{mf * f_scale if mf else mf}")


# Arm: kinematic-faithful tracking is the point of the twin, torque realism is
# not (Phase 0). ×100 stiffness shrinks gravity-sag error 100× (0.015 rad at
# the default → ~1.5e-4, far inside the controller's 0.01 goal tolerance);
# ×100 max force lifts the URDF effort caps (joint3's 32 N·m saturated against
# gravity — the elbow sagged 0.1 rad at REST and stalled 0.33 rad short of a
# -0.5 goal); ×30 damping keeps the PD well-damped without the 1e7-class
# blow-up that crashed PhysX.
# ×10, not ×100: ×10 already shrinks gravity sag to ~0.0015 rad (6× inside
# the 0.01 goal tolerance) and softer drives keep the solver stable near the
# vertical singularity.
scale_drives(robot_prim, {f"joint{i}" for i in range(1, 7)},
             k_scale=10.0, f_scale=100.0)
# Gripper: NO physical DOF in Isaac at all — asset prep locks drive_joint and
# the five mimic fingers to fixed (live gripper joints deterministically
# destabilized the wrist on all-zeros goals); grasping is MoveIt scene-attach.

# Physics at 4× the render/bridge rate: near the vertical pose the wrist axes
# align (j1/j4/j6 gimbal) and the ill-conditioned mass matrix + stiff drives
# go numerically unstable at 60 Hz — joints that should hold still get flung
# 0.1-0.4 rad during home-pose approaches. The ROS graph still ticks per
# render frame (60 Hz states/commands).
world = World(stage_units_in_meters=1.0, physics_dt=1.0 / (4 * SIM_HZ), rendering_dt=1.0 / SIM_HZ)
# NO ground plane: at the xArm6 zero pose the wrist/gripper hangs low enough
# to contact a z=0 floor — the contact constraint shoved j4/j5 ~0.2 rad off
# clean commands during every home-pose approach (overpowering the drives;
# stiffness-independent, deterministic) while offset poses stayed clear. The
# arm is table-mounted in the lab anyway; reintroduce scenery deliberately
# (with clearance) when scene mirroring lands in Phase 1.

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
            # Phase 1 scene mirror: generic subscriber for the motion_bridge
            # world-object snapshot feed (std_msgs/String JSON). The main loop
            # below polls its dynamic outputs:data attribute and reconciles.
            ("MirrorSubscriber", "isaacsim.ros2.bridge.ROS2Subscriber"),
        ],
        keys.CONNECT: [
            ("OnPlaybackTick.outputs:tick", "SubscribeJointState.inputs:execIn"),
            ("OnPlaybackTick.outputs:tick", "PublishJointState.inputs:execIn"),
            ("OnPlaybackTick.outputs:tick", "ArticulationController.inputs:execIn"),
            ("OnPlaybackTick.outputs:tick", "MirrorSubscriber.inputs:execIn"),
            ("Context.outputs:context", "SubscribeJointState.inputs:context"),
            ("Context.outputs:context", "PublishJointState.inputs:context"),
            ("Context.outputs:context", "MirrorSubscriber.inputs:context"),
            ("ReadSimTime.outputs:simulationTime", "PublishJointState.inputs:timeStamp"),
            ("SubscribeJointState.outputs:jointNames", "ArticulationController.inputs:jointNames"),
            # POSITION-ONLY on purpose. Wiring velocityCommand as well makes
            # the drives diverge mid-deceleration (worst on all-zeros goals):
            # the velocity feedforward fights the position target hard enough
            # to fling joints 0.1-0.4 rad off a CLEAN reference (verified with
            # controller_state capture — reference clean, feedback diverging).
            # MoveIt's 0.01 rad goal tolerance + stiff position drives don't
            # need feedforward.
            ("SubscribeJointState.outputs:positionCommand", "ArticulationController.inputs:positionCommand"),
        ],
        keys.SET_VALUES: [
            ("Context.inputs:domain_id", int(os.environ.get("ROS_DOMAIN_ID", "42"))),
            ("SubscribeJointState.inputs:topicName", "/isaac_joint_commands"),
            ("PublishJointState.inputs:topicName", "/isaac_joint_states"),
            ("ArticulationController.inputs:targetPrim", [art_root]),
            ("PublishJointState.inputs:targetPrim", [art_root]),
            ("MirrorSubscriber.inputs:messagePackage", "std_msgs"),
            ("MirrorSubscriber.inputs:messageSubfolder", "msg"),
            ("MirrorSubscriber.inputs:messageName", "String"),
            ("MirrorSubscriber.inputs:topicName", "/aithericon/scene_objects"),
        ],
    },
)

# ── Phase 1 scene mirror: reconcile USD prims against the latest snapshot ──

MIRROR_ROOT = "/World/AithericonMirror"
MIRROR_COLLIDERS = os.environ.get("MIRROR_COLLIDERS", "0") == "1"
MIRROR_COLOR = (0.25, 0.55, 0.95)


def _mirror_prim_name(object_id: str) -> str:
    """Sanitize a MoveIt object id into a legal USD prim name."""
    name = re.sub(r"[^A-Za-z0-9_]", "_", object_id)
    if not name or name[0].isdigit():
        name = "o_" + name
    return name


def _set_mirror_pose(prim, pose: dict, scale) -> None:
    """Idempotently (re)write translate/orient/scale xform ops on `prim`."""
    from pxr import Gf, UsdGeom

    xf = UsdGeom.Xformable(prim)
    ops = {op.GetOpName(): op for op in xf.GetOrderedXformOps()}
    t = ops.get("xformOp:translate") or xf.AddTranslateOp()
    o = ops.get("xformOp:orient") or xf.AddOrientOp()
    s = ops.get("xformOp:scale") or xf.AddScaleOp()
    p = pose.get("position", {})
    q = pose.get("orientation", {})
    t.Set(Gf.Vec3d(float(p.get("x", 0)), float(p.get("y", 0)), float(p.get("z", 0))))
    o.Set(Gf.Quatf(
        float(q.get("w", 1)),
        Gf.Vec3f(float(q.get("x", 0)), float(q.get("y", 0)), float(q.get("z", 0))),
    ))
    s.Set(Gf.Vec3f(*scale))


def reconcile_mirror(snapshot: dict) -> None:
    """Make /World/AithericonMirror match the snapshot's world-object set.

    Declarative + idempotent: create missing prims, delete stale ones, rewrite
    poses in place. Objects listed as `attached` (grasped) are withdrawn from
    the world set while held — the MoveIt scene twin renders them riding the
    gripper; physical contact grasping is a later phase. Poses are taken in
    the robot base frame, which this scene places at the world origin (the
    URDF imports fixed-base at /World with no offset), so no frame transform
    is applied; objects in any OTHER frame are skipped with a warning.
    """
    import omni.usd
    from pxr import UsdGeom, UsdPhysics

    stage = omni.usd.get_context().get_stage()
    stage.DefinePrim(MIRROR_ROOT, "Scope")

    attached = set(snapshot.get("attached", []))
    desired = {}
    for obj in snapshot.get("objects", []):
        if obj.get("id") in attached:
            continue
        if obj.get("frame") not in (None, "", "link_base", "world"):
            print(f"[isaac_xarm_scene] mirror: skipping {obj.get('id')} in "
                  f"unsupported frame {obj.get('frame')}")
            continue
        desired[_mirror_prim_name(str(obj.get("id")))] = obj

    # Delete stale prims (present sim-side, gone MoveIt-side or now attached).
    root = stage.GetPrimAtPath(MIRROR_ROOT)
    removed = 0
    for child in list(root.GetChildren()):
        if child.GetName() not in desired:
            stage.RemovePrim(child.GetPath())
            removed += 1
    created = 0

    # Create/update the desired set.
    for name, obj in desired.items():
        path = f"{MIRROR_ROOT}/{name}"
        primitive = obj.get("primitive")
        dims = [float(d) for d in obj.get("dimensions", [])]
        spec = json.dumps({"primitive": primitive, "dims": dims})

        prim = stage.GetPrimAtPath(path)
        if prim and prim.GetCustomDataByKey("aithericon_spec") != spec:
            # Shape/size changed — rebuild rather than morph.
            stage.RemovePrim(path)
            prim = None

        if not prim:
            if primitive == "box" and len(dims) >= 3:
                geom = UsdGeom.Cube.Define(stage, path)
                geom.GetSizeAttr().Set(1.0)  # unit cube; dims ride the scale op
            elif primitive == "sphere" and len(dims) >= 1:
                geom = UsdGeom.Sphere.Define(stage, path)
                geom.GetRadiusAttr().Set(dims[0])
            elif primitive == "cylinder" and len(dims) >= 2:
                geom = UsdGeom.Cylinder.Define(stage, path)
                # SolidPrimitive CYLINDER dims = [height, radius]; USD axis Z
                # matches MoveIt's cylinder convention.
                geom.GetHeightAttr().Set(dims[0])
                geom.GetRadiusAttr().Set(dims[1])
                geom.GetAxisAttr().Set("Z")
            else:
                print(f"[isaac_xarm_scene] mirror: skipping {obj.get('id')} — "
                      f"unsupported primitive {primitive} dims {dims}")
                continue
            geom.GetDisplayColorAttr().Set([MIRROR_COLOR])
            prim = geom.GetPrim()
            prim.SetCustomDataByKey("aithericon_spec", spec)
            if MIRROR_COLLIDERS:
                # Static collider (no RigidBodyAPI): immovable scenery, the
                # same physics role the lab bench plays.
                UsdPhysics.CollisionAPI.Apply(prim)
            created += 1

        scale = (dims[0], dims[1], dims[2]) if primitive == "box" else (1.0, 1.0, 1.0)
        _set_mirror_pose(prim, obj.get("pose", {}), scale)

    print(f"[isaac_xarm_scene] mirror: reconciled {len(desired)} prims "
          f"(+{created} -{removed}, {len(attached)} attached withheld) "
          f"under {MIRROR_ROOT}")


world.reset()
omni.timeline.get_timeline_interface().play()
print(f"[isaac_xarm_scene] running: {SIM_HZ:.0f} Hz physics, domain {os.environ.get('ROS_DOMAIN_ID', '42')}, "
      f"sub /isaac_joint_commands pub /isaac_joint_states, "
      f"mirror /aithericon/scene_objects (colliders={'on' if MIRROR_COLLIDERS else 'off'})")

# render=True even headless: app updates drive the OmniGraph playback tick (a
# physics-only step would never evaluate the ROS bridge nodes).
#
# The mirror poll rides the same loop: the generic ROS2Subscriber og node
# parks the latest std_msgs/String payload on its dynamic outputs:data
# attribute; we re-parse + reconcile only when the raw payload CHANGES
# (snapshots are latest-wins, so missed intermediates are free). The dynamic
# attribute appears only after the node first initializes — reads before that
# fail harmlessly and we just retry next frame.
_mirror_attr_path = "/ActionGraph/MirrorSubscriber.outputs:data"
_last_mirror_raw = None
while simulation_app.is_running():
    world.step(render=True)
    try:
        raw = og.Controller.get(og.Controller.attribute(_mirror_attr_path))
    except Exception:
        raw = None
    if raw and raw != _last_mirror_raw:
        _last_mirror_raw = raw
        try:
            reconcile_mirror(json.loads(raw))
        except Exception as exc:  # noqa: BLE001 - mirror is best-effort
            print(f"[isaac_xarm_scene] mirror reconcile failed: {exc}")

simulation_app.close()
