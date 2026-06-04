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

import rclpy
from rclpy.callback_groups import ReentrantCallbackGroup
from rclpy.executors import MultiThreadedExecutor
from rclpy.node import Node

from geometry_msgs.msg import PoseStamped
from moveit_msgs.msg import (
    Constraints,
    JointConstraint,
    MotionPlanRequest,
    MoveItErrorCodes,
    PositionIKRequest,
)
from moveit_msgs.srv import GetMotionPlan, GetPositionIK
from rosidl_runtime_py.convert import message_to_ordereddict

from aithericon_motion_bridge.srv import PlanToPose

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

# moveit_msgs/msg/MoveItErrorCodes.SUCCESS == 1, FAILURE == 99999
SUCCESS = MoveItErrorCodes.SUCCESS
FAILURE = MoveItErrorCodes.FAILURE


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

        self._srv = self.create_service(
            PlanToPose,
            "/plan_to_pose",
            self._handle_plan_to_pose,
            callback_group=self._cb_group,
        )

        self.get_logger().info(
            "motion_bridge up: /plan_to_pose -> %s + %s" % (IK_SERVICE, PLAN_SERVICE)
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
