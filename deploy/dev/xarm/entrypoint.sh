#!/usr/bin/env bash
# Dev xArm 6 (fake hardware) + rosbridge launcher. Brings up:
#   - ros2_control with UFRobotFakeSystemHardware (mock hw — no real arm)
#   - MoveIt move_group + the xarm_planner node (xarm_joint_plan /
#     xarm_exec_plan services) + joint_state_broadcaster (/joint_states)
#   - rosapi (interface introspection) + rosbridge_websocket (:9090)
# Headless: launched with show_rviz:=false. An Xvfb virtual display is started
# as a safety net in case any GL/Qt node still probes for a display.
# NOTE: no `set -u` — ROS setup.bash references unbound vars (AMENT_*).
set -eo pipefail

source /opt/ros/jazzy/setup.bash
source /ros2_ws/install/setup.bash

# Safety-net virtual display (we disable RViz, but don't risk a GL probe
# hard-failing the boot).
Xvfb :1 -screen 0 1280x1024x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
export DISPLAY=:1
sleep 1

# Fake xArm 6 planner stack: move_group + the xarm_planner services
# (xarm_joint_plan / xarm_exec_plan) + fake ros2_control + joint_state_broadcaster
# (/joint_states). Background so rosbridge can hold the foreground as PID 1's
# child (the container lifecycle tracks the bridge).
ros2 launch xarm_planner xarm6_planner_fake.launch.py show_rviz:=false no_gui_ctrl:=true \
    >/tmp/xarm.log 2>&1 &

# rosapi (interface introspection: /rosapi/topics, /rosapi/message_details,
# /rosapi/action_*_details, …). Jazzy's `rosbridge_websocket_launch.xml` does
# NOT start rosapi_node, so the runner's catalog publish never resolves unless
# we launch it ourselves.
ros2 run rosapi rosapi_node >/tmp/rosapi.log 2>&1 &

# rosbridge WebSocket (port 9090) in the foreground.
exec ros2 launch rosbridge_server rosbridge_websocket_launch.xml
