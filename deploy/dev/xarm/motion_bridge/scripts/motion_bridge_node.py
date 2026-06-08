#!/usr/bin/env python3
"""Path C motion-bridge node for the dev xArm 6 (fake) image.

Re-exposes MoveIt move_group planning as a flat ``/plan_to_pose`` service that
RETURNS the planned trajectory. The stock ``xarm_pose_plan`` stashes the plan
internally (for ``xarm_exec_plan``) and returns only ``{success}``; the platform
needs the trajectory back as a referenceable output so the demo can splice it
verbatim into Path B's ``FollowJointTrajectory`` action goal.

``moveit_py`` / ``ros-jazzy-moveit-py`` are NOT installed in this image, so the
C++ ``MoveGroupInterface`` / ``constructGoalConstraints`` helpers are out. This
is a plain ``rclpy`` node that calls move_group's OWN services:

  1. ``/compute_ik``  (moveit_msgs/srv/GetPositionIK)   -> joint solution
  2. ``/plan_kinematic_path`` (moveit_msgs/srv/GetMotionPlan) with hand-built
     ``JointConstraint``s from the IK solution.

The INNER ``motion_plan_response.trajectory.joint_trajectory`` (a
``trajectory_msgs/JointTrajectory``) is serialized to JSON and returned in
``trajectory`` so it drops straight into ``FollowJointTrajectory.goal.trajectory``
with zero reshaping.

Threading: the service callback synchronously ``.call()``s the move_group
clients. A single-threaded executor would deadlock (the callback occupies the
only spin thread, so the client responses never get processed). We therefore use
a MultiThreadedExecutor and put every client + the service on a
ReentrantCallbackGroup.

Metrics: top-level numeric response fields are NOT nullable in the derived port,
so every failure path returns FINITE sentinels (0 / 0.0), never NaN.
"""

import json
import time

import rclpy
from rclpy.action import ActionClient
from rclpy.callback_groups import ReentrantCallbackGroup
from rclpy.executors import MultiThreadedExecutor
from rclpy.node import Node

from builtin_interfaces.msg import Duration
from control_msgs.action import FollowJointTrajectory
from geometry_msgs.msg import PoseStamped
from moveit_msgs.msg import (
    AttachedCollisionObject,
    CollisionObject,
    Constraints,
    JointConstraint,
    MotionPlanRequest,
    MoveItErrorCodes,
    PlanningScene,
    PlanningSceneComponents,
    PositionIKRequest,
)
from moveit_msgs.srv import (
    ApplyPlanningScene,
    GetCartesianPath,
    GetMotionPlan,
    GetPlanningScene,
    GetPositionIK,
)
from shape_msgs.msg import SolidPrimitive
from trajectory_msgs.msg import JointTrajectory, JointTrajectoryPoint
from rosidl_runtime_py.convert import message_to_ordereddict

from aithericon_motion_bridge.srv import (
    AddObject,
    ClearScene,
    Grasp,
    PlanCartesian,
    PlanToPose,
    Release,
    RemoveObject,
)

# The six xArm 6 arm joints. The IK solution's joint_state also carries the
# gripper joint (drive_joint); we filter it out of the JointConstraints so the
# plan targets only the arm of the "xarm6" SRDF planning group.
ARM_JOINTS = (
    "joint1",
    "joint2",
    "joint3",
    "joint4",
    "joint5",
    "joint6",
)

DEFAULT_GROUP = "xarm6"
DEFAULT_FRAME = "link_base"
DEFAULT_PLANNING_TIME = 5.0
DEFAULT_ATTEMPTS = 10

IK_SERVICE = "/compute_ik"
PLAN_SERVICE = "/plan_kinematic_path"

# S2: scene mutation + Cartesian planning services exposed by move_group.
APPLY_SCENE_SERVICE = "/apply_planning_scene"
GET_SCENE_SERVICE = "/get_planning_scene"
CARTESIAN_SERVICE = "/compute_cartesian_path"

# S2 PlanCartesian defaults.
DEFAULT_EEF_STEP = 0.01
DEFAULT_JUMP_THRESHOLD = 0.0

# S3 grasp/release tuning (easy to change for a real cell).
#   ATTACH_LINK   — the gripper TCP link the AttachedCollisionObject rides on.
#   GRIPPER_ACTION — FollowJointTrajectory controller for best-effort actuation.
#   GRIPPER_JOINT  — the actuated gripper joint.
ATTACH_LINK = "link_tcp"
# Links allowed to TOUCH the grasped object without it counting as a collision.
# Without these, MoveIt treats the closed gripper fingers contacting the attached
# sample as a self-collision, so EVERY plan after the grasp fails from the start
# state (the retreat lift solves fraction 0.0; the next job's /compute_ik returns
# NO_IK_SOLUTION). Listing the whole gripper link set as touch_links is the
# standard MoveIt grasp practice and unblocks retreat + move-to-place + place.
GRIPPER_TOUCH_LINKS = [
    "link_tcp",
    "link_eef",
    "xarm_gripper_base_link",
    "left_outer_knuckle",
    "left_inner_knuckle",
    "left_finger",
    "right_outer_knuckle",
    "right_inner_knuckle",
    "right_finger",
]
GRIPPER_ACTION = "/xarm_gripper_traj_controller/follow_joint_trajectory"
GRIPPER_JOINT = "drive_joint"
# How long to wait for the (optional) gripper action server before giving up and
# proceeding with the scene attach/detach only.
GRIPPER_WAIT_SEC = 10.0
GRIPPER_MOVE_TIME_SEC = 1.5
# drive_joint travel: 0.0 == fully OPEN, ~0.85 == fully CLOSED on the UFACTORY
# gripper. grasp drives toward CLOSED, release back to OPEN. (The Grasp.srv
# `width` is a real-cell hint; in the fake sim we close fully for a visible grip.)
GRIPPER_OPEN = 0.0
GRIPPER_CLOSED = 0.8

# moveit_msgs/msg/MoveItErrorCodes.SUCCESS == 1, FAILURE == 99999
SUCCESS = MoveItErrorCodes.SUCCESS
FAILURE = MoveItErrorCodes.FAILURE

# Map the AddObject `primitive` string -> shape_msgs/SolidPrimitive type constant.
_PRIMITIVE_TYPES = {
    "box": SolidPrimitive.BOX,
    "sphere": SolidPrimitive.SPHERE,
    "cylinder": SolidPrimitive.CYLINDER,
}


class MotionBridge(Node):
    def __init__(self) -> None:
        super().__init__("motion_bridge")

        # One reentrant group shared by the service callback and the clients so a
        # synchronous .call() inside the callback can be serviced concurrently by
        # the MultiThreadedExecutor (otherwise: deadlock).
        self._cb_group = ReentrantCallbackGroup()

        self._ik_client = self.create_client(
            GetPositionIK, IK_SERVICE, callback_group=self._cb_group
        )
        self._plan_client = self.create_client(
            GetMotionPlan, PLAN_SERVICE, callback_group=self._cb_group
        )
        # S2: scene mutation + Cartesian planning clients (same reentrant group).
        self._apply_scene_client = self.create_client(
            ApplyPlanningScene, APPLY_SCENE_SERVICE, callback_group=self._cb_group
        )
        self._get_scene_client = self.create_client(
            GetPlanningScene, GET_SCENE_SERVICE, callback_group=self._cb_group
        )
        self._cartesian_client = self.create_client(
            GetCartesianPath, CARTESIAN_SERVICE, callback_group=self._cb_group
        )
        # S3: best-effort gripper actuation action client (same reentrant group).
        self._gripper_action = ActionClient(
            self, FollowJointTrajectory, GRIPPER_ACTION,
            callback_group=self._cb_group,
        )

        self._srv = self.create_service(
            PlanToPose,
            "/plan_to_pose",
            self._handle_plan_to_pose,
            callback_group=self._cb_group,
        )
        # S2 services.
        self._add_object_srv = self.create_service(
            AddObject, "/add_object", self._handle_add_object,
            callback_group=self._cb_group,
        )
        self._remove_object_srv = self.create_service(
            RemoveObject, "/remove_object", self._handle_remove_object,
            callback_group=self._cb_group,
        )
        self._clear_scene_srv = self.create_service(
            ClearScene, "/clear_scene", self._handle_clear_scene,
            callback_group=self._cb_group,
        )
        self._plan_cartesian_srv = self.create_service(
            PlanCartesian, "/plan_cartesian", self._handle_plan_cartesian,
            callback_group=self._cb_group,
        )
        # S3 services.
        self._grasp_srv = self.create_service(
            Grasp, "/grasp", self._handle_grasp,
            callback_group=self._cb_group,
        )
        self._release_srv = self.create_service(
            Release, "/release", self._handle_release,
            callback_group=self._cb_group,
        )

        self.get_logger().info(
            "motion_bridge up: "
            "/plan_to_pose,/plan_cartesian -> %s,%s,%s ; "
            "/add_object,/remove_object,/clear_scene -> %s,%s ; "
            "/grasp,/release -> %s + %s"
            % (
                IK_SERVICE,
                PLAN_SERVICE,
                CARTESIAN_SERVICE,
                APPLY_SCENE_SERVICE,
                GET_SCENE_SERVICE,
                APPLY_SCENE_SERVICE,
                GRIPPER_ACTION,
            )
        )

    # ---- helpers ---------------------------------------------------------

    @staticmethod
    def _fail(resp, code, message):
        """Fill a response with FINITE sentinels on a failure path."""
        resp.success = False
        resp.trajectory = ""
        resp.planning_time = 0.0
        resp.point_count = 0
        resp.total_duration = 0.0
        resp.error_code = int(code)
        resp.error_message = message
        return resp

    def _ensure_clients(self, resp):
        """Wait for both move_group services so launch order is forgiving.

        Returns None on success, or a filled failure response if a service
        never appeared.
        """
        if not self._ik_client.wait_for_service(timeout_sec=10.0):
            return self._fail(resp, FAILURE, "compute_ik service unavailable")
        if not self._plan_client.wait_for_service(timeout_sec=10.0):
            return self._fail(
                resp, FAILURE, "plan_kinematic_path service unavailable"
            )
        return None

    # ---- service handler -------------------------------------------------

    def _handle_plan_to_pose(self, req, resp):
        group = req.group or DEFAULT_GROUP
        frame = req.frame_id or DEFAULT_FRAME
        planning_time = (
            req.allowed_planning_time
            if req.allowed_planning_time > 0.0
            else DEFAULT_PLANNING_TIME
        )
        attempts = (
            req.num_planning_attempts
            if req.num_planning_attempts > 0
            else DEFAULT_ATTEMPTS
        )

        not_ready = self._ensure_clients(resp)
        if not_ready is not None:
            return not_ready

        # --- 1. IK: target Pose -> joint values -------------------------
        ik_req = GetPositionIK.Request()
        ik_req.ik_request = PositionIKRequest()
        ik_req.ik_request.group_name = group
        ik_req.ik_request.avoid_collisions = True
        pose_stamped = PoseStamped()
        pose_stamped.header.frame_id = frame
        pose_stamped.header.stamp = self.get_clock().now().to_msg()
        pose_stamped.pose = req.target
        ik_req.ik_request.pose_stamped = pose_stamped

        try:
            ik_resp = self._ik_client.call(ik_req)
        except Exception as exc:  # noqa: BLE001 - report any RMW/call error finitely
            return self._fail(resp, FAILURE, "compute_ik call error: %s" % exc)

        if ik_resp is None:
            return self._fail(resp, FAILURE, "compute_ik returned no response")

        if ik_resp.error_code.val != SUCCESS:
            return self._fail(
                resp,
                ik_resp.error_code.val,
                "IK failed (MoveItErrorCodes.val=%d)" % ik_resp.error_code.val,
            )

        joint_state = ik_resp.solution.joint_state
        # Build joint constraints for ONLY the arm joints (drop the gripper).
        joint_constraints = []
        for name, position in zip(joint_state.name, joint_state.position):
            if name not in ARM_JOINTS:
                continue
            jc = JointConstraint()
            jc.joint_name = name
            jc.position = float(position)
            jc.tolerance_above = 1e-3
            jc.tolerance_below = 1e-3
            jc.weight = 1.0
            joint_constraints.append(jc)

        if not joint_constraints:
            return self._fail(
                resp,
                FAILURE,
                "IK solution carried no arm joints (expected one of %s)"
                % (", ".join(ARM_JOINTS)),
            )

        # --- 2. Plan: joint-goal MotionPlanRequest ----------------------
        mpr = MotionPlanRequest()
        mpr.group_name = group
        mpr.num_planning_attempts = int(attempts)
        mpr.allowed_planning_time = float(planning_time)
        goal = Constraints()
        goal.joint_constraints = joint_constraints
        mpr.goal_constraints = [goal]

        plan_req = GetMotionPlan.Request()
        plan_req.motion_plan_request = mpr

        try:
            plan_resp = self._plan_client.call(plan_req)
        except Exception as exc:  # noqa: BLE001
            return self._fail(
                resp, FAILURE, "plan_kinematic_path call error: %s" % exc
            )

        if plan_resp is None:
            return self._fail(
                resp, FAILURE, "plan_kinematic_path returned no response"
            )

        mp = plan_resp.motion_plan_response
        if mp.error_code.val != SUCCESS:
            return self._fail(
                resp,
                mp.error_code.val,
                "planning failed (MoveItErrorCodes.val=%d)" % mp.error_code.val,
            )

        # --- 3. Serialize the INNER JointTrajectory ---------------------
        joint_traj = mp.trajectory.joint_trajectory
        points = list(joint_traj.points)
        try:
            traj_json = json.dumps(message_to_ordereddict(joint_traj))
        except Exception as exc:  # noqa: BLE001
            return self._fail(
                resp, FAILURE, "trajectory serialization failed: %s" % exc
            )

        if points:
            last = points[-1].time_from_start
            total_duration = float(last.sec) + float(last.nanosec) / 1e9
        else:
            total_duration = 0.0

        resp.success = True
        resp.trajectory = traj_json
        resp.planning_time = float(mp.planning_time)
        resp.point_count = len(points)
        resp.total_duration = total_duration
        resp.error_code = SUCCESS
        resp.error_message = ""
        self.get_logger().info(
            "plan_to_pose ok: points=%d planning_time=%.3fs total_duration=%.3fs"
            % (resp.point_count, resp.planning_time, resp.total_duration)
        )
        return resp

    # ---- S2: scene mutation helpers -------------------------------------

    @staticmethod
    def _scene_fail(resp, message):
        """Fill a scene-op response (AddObject/RemoveObject) with finite sentinels."""
        resp.success = False
        resp.error_message = message
        return resp

    def _apply_scene_diff(self, planning_scene):
        """Call /apply_planning_scene with a diff. Returns (ok, message)."""
        if not self._apply_scene_client.wait_for_service(timeout_sec=10.0):
            return False, "%s service unavailable" % APPLY_SCENE_SERVICE
        scene_req = ApplyPlanningScene.Request()
        scene_req.scene = planning_scene
        try:
            scene_resp = self._apply_scene_client.call(scene_req)
        except Exception as exc:  # noqa: BLE001
            return False, "%s call error: %s" % (APPLY_SCENE_SERVICE, exc)
        if scene_resp is None:
            return False, "%s returned no response" % APPLY_SCENE_SERVICE
        if not scene_resp.success:
            return False, "%s rejected the scene diff" % APPLY_SCENE_SERVICE
        return True, ""

    def _handle_add_object(self, req, resp):
        prim_key = (req.primitive or "").strip().lower()
        if prim_key not in _PRIMITIVE_TYPES:
            return self._scene_fail(
                resp,
                "unknown primitive '%s' (expected box|sphere|cylinder)"
                % req.primitive,
            )
        if not req.object_id:
            return self._scene_fail(resp, "object_id is required")

        frame = req.pose.header.frame_id or DEFAULT_FRAME

        primitive = SolidPrimitive()
        primitive.type = _PRIMITIVE_TYPES[prim_key]
        primitive.dimensions = [float(d) for d in req.dimensions]

        obj = CollisionObject()
        obj.id = req.object_id
        obj.header.frame_id = frame
        obj.header.stamp = self.get_clock().now().to_msg()
        obj.operation = CollisionObject.ADD
        obj.primitives = [primitive]
        obj.primitive_poses = [req.pose.pose]

        scene = PlanningScene()
        scene.is_diff = True
        scene.world.collision_objects = [obj]

        ok, message = self._apply_scene_diff(scene)
        if not ok:
            return self._scene_fail(resp, message)

        resp.success = True
        resp.error_message = ""
        self.get_logger().info(
            "add_object ok: id=%s primitive=%s frame=%s"
            % (req.object_id, prim_key, frame)
        )
        return resp

    def _handle_remove_object(self, req, resp):
        if not req.object_id:
            return self._scene_fail(resp, "object_id is required")

        obj = CollisionObject()
        obj.id = req.object_id
        obj.header.frame_id = DEFAULT_FRAME
        obj.header.stamp = self.get_clock().now().to_msg()
        obj.operation = CollisionObject.REMOVE

        scene = PlanningScene()
        scene.is_diff = True
        scene.world.collision_objects = [obj]

        ok, message = self._apply_scene_diff(scene)
        if not ok:
            return self._scene_fail(resp, message)

        resp.success = True
        resp.error_message = ""
        self.get_logger().info("remove_object ok: id=%s" % req.object_id)
        return resp

    def _handle_clear_scene(self, req, resp):
        # Finite sentinels on every failure path (removed_count is not nullable).
        if not req.confirm:
            resp.success = False
            resp.removed_count = 0
            resp.error_message = "clear_scene requires confirm == true"
            return resp

        if not self._get_scene_client.wait_for_service(timeout_sec=10.0):
            resp.success = False
            resp.removed_count = 0
            resp.error_message = "%s service unavailable" % GET_SCENE_SERVICE
            return resp

        get_req = GetPlanningScene.Request()
        # Request BOTH world objects AND attached objects — otherwise the attached
        # set comes back empty and clear_scene silently leaves grasped samples
        # stuck on the gripper (its detach loop below iterates nothing).
        get_req.components.components = (
            PlanningSceneComponents.WORLD_OBJECT_NAMES
            | PlanningSceneComponents.ROBOT_STATE_ATTACHED_OBJECTS
        )
        try:
            get_resp = self._get_scene_client.call(get_req)
        except Exception as exc:  # noqa: BLE001
            resp.success = False
            resp.removed_count = 0
            resp.error_message = "%s call error: %s" % (GET_SCENE_SERVICE, exc)
            return resp
        if get_resp is None:
            resp.success = False
            resp.removed_count = 0
            resp.error_message = "%s returned no response" % GET_SCENE_SERVICE
            return resp

        world = get_resp.scene.world
        remove_objects = []
        for existing in world.collision_objects:
            obj = CollisionObject()
            obj.id = existing.id
            obj.header.frame_id = existing.header.frame_id or DEFAULT_FRAME
            obj.operation = CollisionObject.REMOVE
            remove_objects.append(obj)

        # Detach any attached objects too (they go back to the world, then removed).
        detach_objects = []
        for attached in get_resp.scene.robot_state.attached_collision_objects:
            aco = AttachedCollisionObject()
            aco.link_name = attached.link_name
            aco.object.id = attached.object.id
            aco.object.operation = CollisionObject.REMOVE
            detach_objects.append(aco)
            # And remove from world after detach.
            obj = CollisionObject()
            obj.id = attached.object.id
            obj.operation = CollisionObject.REMOVE
            remove_objects.append(obj)

        removed_count = len(world.collision_objects) + len(detach_objects)

        if removed_count == 0:
            resp.success = True
            resp.removed_count = 0
            resp.error_message = ""
            self.get_logger().info("clear_scene ok: scene already empty")
            return resp

        scene = PlanningScene()
        scene.is_diff = True
        scene.world.collision_objects = remove_objects
        scene.robot_state.is_diff = True
        scene.robot_state.attached_collision_objects = detach_objects

        ok, message = self._apply_scene_diff(scene)
        if not ok:
            resp.success = False
            resp.removed_count = 0
            resp.error_message = message
            return resp

        resp.success = True
        resp.removed_count = int(removed_count)
        resp.error_message = ""
        self.get_logger().info(
            "clear_scene ok: removed=%d" % resp.removed_count
        )
        return resp

    # ---- S2: Cartesian planning -----------------------------------------

    @staticmethod
    def _cartesian_fail(resp, code, message):
        """Fill a PlanCartesian response with FINITE sentinels on failure."""
        resp.success = False
        resp.trajectory = ""
        resp.planning_time = 0.0
        resp.point_count = 0
        resp.total_duration = 0.0
        resp.fraction = 0.0
        resp.error_code = int(code)
        resp.error_message = message
        return resp

    def _handle_plan_cartesian(self, req, resp):
        group = req.group or DEFAULT_GROUP
        frame = req.frame_id or DEFAULT_FRAME
        eef_step = req.eef_step if req.eef_step > 0.0 else DEFAULT_EEF_STEP
        jump_threshold = (
            req.jump_threshold
            if req.jump_threshold > 0.0
            else DEFAULT_JUMP_THRESHOLD
        )

        if not list(req.waypoints):
            return self._cartesian_fail(resp, FAILURE, "waypoints is empty")

        if not self._cartesian_client.wait_for_service(timeout_sec=10.0):
            return self._cartesian_fail(
                resp, FAILURE, "%s service unavailable" % CARTESIAN_SERVICE
            )

        cart_req = GetCartesianPath.Request()
        cart_req.group_name = group
        cart_req.header.frame_id = frame
        cart_req.header.stamp = self.get_clock().now().to_msg()
        cart_req.waypoints = list(req.waypoints)
        cart_req.max_step = float(eef_step)
        cart_req.jump_threshold = float(jump_threshold)
        cart_req.avoid_collisions = True

        try:
            cart_resp = self._cartesian_client.call(cart_req)
        except Exception as exc:  # noqa: BLE001
            return self._cartesian_fail(
                resp, FAILURE, "%s call error: %s" % (CARTESIAN_SERVICE, exc)
            )
        if cart_resp is None:
            return self._cartesian_fail(
                resp, FAILURE, "%s returned no response" % CARTESIAN_SERVICE
            )

        if cart_resp.error_code.val != SUCCESS:
            return self._cartesian_fail(
                resp,
                cart_resp.error_code.val,
                "cartesian planning failed (MoveItErrorCodes.val=%d)"
                % cart_resp.error_code.val,
            )

        joint_traj = cart_resp.solution.joint_trajectory
        points = list(joint_traj.points)
        # /compute_cartesian_path returns a GEOMETRIC path: every point carries
        # time_from_start=0 and no velocities, which a FollowJointTrajectory
        # controller cannot execute (all points "due" at t=0 -> rejected / instant
        # jump). Stamp a monotonic time parameterization: advance time per segment
        # by the largest joint displacement / a nominal joint speed, floored so
        # timestamps strictly increase, and clear velocities/accelerations so the
        # controller interpolates. Adequate for the fake joint_trajectory_controller
        # on these slow fine-approach (grasp descend / retreat) motions.
        nominal_speed = 0.5  # rad/s
        min_dt = 0.05  # s — floor so consecutive points never share a timestamp
        elapsed = 0.0
        prev = None
        for pt in points:
            if prev is not None:
                max_delta = max(
                    (abs(a - b) for a, b in zip(pt.positions, prev.positions)),
                    default=0.0,
                )
                elapsed += max(max_delta / nominal_speed, min_dt)
            pt.time_from_start.sec = int(elapsed)
            pt.time_from_start.nanosec = int(round((elapsed - int(elapsed)) * 1e9))
            pt.velocities = []
            pt.accelerations = []
            prev = pt
        try:
            traj_json = json.dumps(message_to_ordereddict(joint_traj))
        except Exception as exc:  # noqa: BLE001
            return self._cartesian_fail(
                resp, FAILURE, "trajectory serialization failed: %s" % exc
            )

        if points:
            last = points[-1].time_from_start
            total_duration = float(last.sec) + float(last.nanosec) / 1e9
        else:
            total_duration = 0.0

        fraction = float(cart_resp.fraction)
        # Guard against any non-finite leaking into a non-nullable port.
        if fraction != fraction:  # NaN
            fraction = 0.0

        resp.success = True
        resp.trajectory = traj_json
        resp.planning_time = 0.0  # GetCartesianPath carries no planning_time field
        resp.point_count = len(points)
        resp.total_duration = total_duration
        resp.fraction = fraction
        resp.error_code = SUCCESS
        resp.error_message = ""
        self.get_logger().info(
            "plan_cartesian ok: points=%d fraction=%.3f total_duration=%.3fs"
            % (resp.point_count, resp.fraction, resp.total_duration)
        )
        return resp

    # ---- S3: atomic grasp / release (gripper actuation + scene attach) ---

    @staticmethod
    def _await_future(future, timeout_sec):
        """Block on a future from a worker-thread callback WITHOUT re-spinning.

        The MultiThreadedExecutor owns the node's spin loop; calling
        ``spin_until_future_complete`` from inside a callback would re-enter the
        executor. Instead we poll ``future.done()`` -- the executor's own threads
        keep servicing the underlying action responses. Returns True if completed.
        """
        deadline = time.monotonic() + timeout_sec
        while not future.done():
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.02)
        return True

    def _actuate_gripper(self, position):
        """Best-effort gripper move via FollowJointTrajectory. Never raises.

        Returns (ok, note). On any unavailability/error returns (False, note) and
        the caller proceeds with the scene op anyway (fake sim has no real gripper).
        """
        if not self._gripper_action.wait_for_server(timeout_sec=GRIPPER_WAIT_SEC):
            return False, "gripper action server %s unavailable" % GRIPPER_ACTION

        goal = FollowJointTrajectory.Goal()
        traj = JointTrajectory()
        traj.joint_names = [GRIPPER_JOINT]
        point = JointTrajectoryPoint()
        point.positions = [float(position)]
        point.time_from_start = Duration(sec=int(GRIPPER_MOVE_TIME_SEC), nanosec=0)
        traj.points = [point]
        goal.trajectory = traj

        try:
            send_future = self._gripper_action.send_goal_async(goal)
            if not self._await_future(send_future, GRIPPER_WAIT_SEC):
                return False, "gripper goal send timed out"
            handle = send_future.result()
            if handle is None or not handle.accepted:
                return False, "gripper goal rejected"
            result_future = handle.get_result_async()
            if not self._await_future(result_future, GRIPPER_MOVE_TIME_SEC + 2.0):
                return False, "gripper result timed out"
        except Exception as exc:  # noqa: BLE001
            return False, "gripper actuation error: %s" % exc
        return True, ""

    def _attach_diff(self, object_id, operation):
        """Apply an AttachedCollisionObject ADD/REMOVE diff. Returns (ok, message)."""
        aco = AttachedCollisionObject()
        aco.link_name = ATTACH_LINK
        # Allow the gripper links to touch the attached body — otherwise the
        # closed fingers around the sample read as a self-collision and block
        # every subsequent plan (retreat / move-to-place IK).
        aco.touch_links = list(GRIPPER_TOUCH_LINKS)
        aco.object.id = object_id
        aco.object.header.frame_id = ATTACH_LINK
        aco.object.header.stamp = self.get_clock().now().to_msg()
        aco.object.operation = operation

        scene = PlanningScene()
        scene.is_diff = True
        scene.robot_state.is_diff = True
        scene.robot_state.attached_collision_objects = [aco]

        return self._apply_scene_diff(scene)

    def _attached_ids(self):
        """Read every object currently attached to the robot as (link, id) pairs.

        Returns None if the scene can't be read (caller treats as 'unknown')."""
        if not self._get_scene_client.wait_for_service(timeout_sec=5.0):
            return None
        get_req = GetPlanningScene.Request()
        get_req.components.components = (
            PlanningSceneComponents.ROBOT_STATE_ATTACHED_OBJECTS
        )
        try:
            get_resp = self._get_scene_client.call(get_req)
        except Exception:  # noqa: BLE001
            return None
        if get_resp is None:
            return None
        return [
            (a.link_name, a.object.id)
            for a in get_resp.scene.robot_state.attached_collision_objects
        ]

    def _detach_all_attached(self, retries=4):
        """Detach EVERY attached object back into the world, VERIFYING it stuck.

        Robustness backstop. A single AttachedCollisionObject REMOVE diff
        occasionally does not take effect under rapid scene churn (a place's
        detach immediately followed by retract motion + the twin's concurrent
        /get_planning_scene polling), leaving a sample stuck on the gripper.
        A stuck body then makes every SUBSEQUENT pick's /compute_ik read the
        held object as an in-collision start state, so the whole rest of a
        multi-sample run silently fails. Re-querying and re-applying the detach
        until the gripper reads empty makes release self-healing. Detaches ALL
        attached ids (not just one) so a stuck PRIOR sample is also cleared.
        """
        last = None
        for _ in range(max(1, retries)):
            attached = self._attached_ids()
            if attached is None:
                return False, "could not read attached objects"
            if not attached:
                return True, ""
            last = attached
            diffs = []
            for link_name, obj_id in attached:
                aco = AttachedCollisionObject()
                aco.link_name = link_name
                aco.object.id = obj_id
                aco.object.operation = CollisionObject.REMOVE
                diffs.append(aco)
            scene = PlanningScene()
            scene.is_diff = True
            scene.robot_state.is_diff = True
            scene.robot_state.attached_collision_objects = diffs
            self._apply_scene_diff(scene)
            # Let the planning-scene monitor catch up before re-reading.
            time.sleep(0.15)
        attached = self._attached_ids()
        if attached:
            return False, "still attached after %d retries: %s" % (retries, last)
        return True, ""

    def _handle_grasp(self, req, resp):
        if not req.object_id:
            resp.success = False
            resp.attached = False
            resp.error_message = "object_id is required"
            return resp

        # 1. Best-effort gripper close (drive toward CLOSED; non-fatal).
        gripper_ok, gripper_note = self._actuate_gripper(GRIPPER_CLOSED)
        if not gripper_ok:
            self.get_logger().warning(
                "grasp: gripper actuation skipped (%s); proceeding with attach"
                % gripper_note
            )

        # 2. The part that MATTERS: attach the object to the gripper TCP link.
        ok, message = self._attach_diff(req.object_id, CollisionObject.ADD)
        if not ok:
            resp.success = False
            resp.attached = False
            resp.error_message = "attach failed: %s" % message
            return resp

        resp.success = True
        resp.attached = True
        resp.error_message = (
            "" if gripper_ok else "gripper actuation skipped: %s" % gripper_note
        )
        self.get_logger().info(
            "grasp ok: id=%s attached on %s%s"
            % (
                req.object_id,
                ATTACH_LINK,
                "" if gripper_ok else " (gripper skipped)",
            )
        )
        return resp

    def _handle_release(self, req, resp):
        if not req.object_id:
            resp.success = False
            resp.attached = False
            resp.error_message = "object_id is required"
            return resp

        # 1. The part that MATTERS: detach the object back into the world. Use the
        #    self-healing detach-ALL + verify path rather than a single one-shot
        #    diff — a lone REMOVE intermittently doesn't stick under the demo's
        #    rapid place->retract->poll churn, stranding a sample on the gripper
        #    that then blocks every later pick. In this cell only one object is
        #    ever held, so detaching all is equivalent for the named release and
        #    additionally clears any stuck prior sample.
        ok, message = self._detach_all_attached()
        if not ok:
            resp.success = False
            # Attach state unknown on failure; report still-attached conservatively.
            resp.attached = True
            resp.error_message = "detach failed: %s" % message
            return resp

        # 2. Best-effort gripper open (non-fatal).
        gripper_ok, gripper_note = self._actuate_gripper(GRIPPER_OPEN)
        if not gripper_ok:
            self.get_logger().warning(
                "release: gripper actuation skipped (%s); detach already applied"
                % gripper_note
            )

        resp.success = True
        resp.attached = False
        resp.error_message = (
            "" if gripper_ok else "gripper actuation skipped: %s" % gripper_note
        )
        self.get_logger().info(
            "release ok: id=%s detached from %s%s"
            % (
                req.object_id,
                ATTACH_LINK,
                "" if gripper_ok else " (gripper skipped)",
            )
        )
        return resp


def main(args=None) -> None:
    rclpy.init(args=args)
    node = MotionBridge()
    # MultiThreadedExecutor: the synchronous .call() inside the service callback
    # would deadlock a single-threaded executor.
    executor = MultiThreadedExecutor()
    executor.add_node(node)
    try:
        executor.spin()
    except KeyboardInterrupt:
        pass
    finally:
        node.destroy_node()
        if rclpy.ok():
            rclpy.shutdown()


if __name__ == "__main__":
    main()
