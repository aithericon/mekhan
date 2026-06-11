#!/usr/bin/env bash
# Dev xArm 6 (fake hardware) + rosbridge launcher. Brings up:
#   - ros2_control with UFRobotFakeSystemHardware (mock hw — no real arm)
#   - MoveIt move_group + the xarm_planner node (xarm_joint_plan /
#     xarm_exec_plan services) + joint_state_broadcaster (/joint_states)
#   - rosapi (interface introspection) + rosbridge_websocket (:9090)
# A browser viewer (noVNC) exports RViz so a human can WATCH the 3D arm move:
# RViz renders on the Xvfb display, x11vnc exports it, websockify serves noVNC.
# NOTE: no `set -u` — ROS setup.bash references unbound vars (AMENT_*).
set -eo pipefail

source /opt/ros/jazzy/setup.bash
source /ros2_ws/install/setup.bash

# Virtual display for RViz's OpenGL window. Software GL (llvmpipe) is forced via
# LIBGL_ALWAYS_SOFTWARE=1 (baked in the image) since there's no GPU here.
Xvfb :1 -screen 0 1280x1024x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
export DISPLAY=:1
sleep 1

# Tiny window manager so RViz maps + maximizes into a usable desktop instead of
# a bare undecorated floating window. Best-effort — never block the stack below.
fluxbox >/tmp/fluxbox.log 2>&1 &

# Browser viewer (mirrors the turtlesim harness): x11vnc exports the Xvfb :1
# display over RFB (:5900); websockify wraps it as a noVNC web client on :6080,
# so a human can open http://localhost:<host-port>/vnc.html and WATCH the 3D arm
# move while a ros-backed demo runs. Best-effort — failures here never block the
# rosbridge below. `-bg` daemonizes x11vnc; `|| true` keeps `set -e` happy.
x11vnc -display :1 -forever -shared -nopw -rfbport 5900 -bg -quiet >/tmp/x11vnc.log 2>&1 || true
websockify --web=/usr/share/novnc 6080 localhost:5900 >/tmp/novnc.log 2>&1 &

# xArm 6 planner stack: move_group + the xarm_planner services
# (xarm_joint_plan / xarm_exec_plan) + ros2_control + joint_state_broadcaster
# (/joint_states) + RViz (the MoveIt MotionPlanning display, on DISPLAY=:1).
# Background so rosbridge can hold the foreground as PID 1's child (the container
# lifecycle tracks the bridge).
# HW_BACKEND picks the ros2_control hardware seam (launch wrappers baked in the
# image): fake (default) = UFRobotFakeSystemHardware mock, self-contained;
# isaac = topic_based_ros2_control bridging /isaac_joint_commands +
# /isaac_joint_states to an Isaac Sim container on the same DDS domain
# (deploy/sim/isaac/) — same stack, physics execution.
HW_BACKEND="${HW_BACKEND:-fake}"
ros2 launch xarm_planner "xarm6_planner_${HW_BACKEND}.launch.py" show_rviz:=true no_gui_ctrl:=true \
    >/tmp/xarm.log 2>&1 &

# Path C motion-bridge (re-exposes move_group planning as /plan_to_pose). The
# node create_client-waits for /compute_ik + /plan_kinematic_path (10s each), so
# launching it here — best-effort, backgrounded, alongside the planner stack — is
# timing-forgiving. The /ros2_ws/install overlay sourced at the top of this
# entrypoint carries the bridge, so `ros2 run` resolves it.
ros2 run aithericon_motion_bridge motion_bridge_node.py >/tmp/motion_bridge.log 2>&1 &

# Maximize the RViz window so the noVNC view is a full-viewport RViz framed on
# the arm, not a small floating window. Best-effort, backgrounded. Under
# software GL (llvmpipe) RViz takes ~30-40s to finish mapping + its own layout
# pass, and an early resize does NOT stick — so don't resize once and stop;
# CONTINUOUSLY re-find the window and re-apply the size for ~2 min. Once RViz
# settles the size sticks and the remaining re-applies are harmless no-ops.
( for _ in $(seq 1 40); do
    # Match ONLY the main RViz window — its title carries the config path
    # ("…/planner.rviz - RViz"); the helper windows ("rviz2", "Qt Selection
    # Owner for rviz2") do not, so don't match on the bare "RViz" substring.
    wid="$(DISPLAY=:1 xdotool search --name 'planner.rviz' 2>/dev/null | tail -1)"
    if [ -n "$wid" ]; then
      DISPLAY=:1 xdotool windowsize "$wid" 1278 1004 windowmove "$wid" 1 2 2>/dev/null || true
    fi
    sleep 3
  done ) >/tmp/maximize.log 2>&1 &

# rosapi (interface introspection: /rosapi/topics, /rosapi/message_details,
# /rosapi/action_*_details, …). Jazzy's `rosbridge_websocket_launch.xml` does
# NOT start rosapi_node, so the runner's catalog publish never resolves unless
# we launch it ourselves.
ros2 run rosapi rosapi_node >/tmp/rosapi.log 2>&1 &

# rosbridge WebSocket (port 9090) in the foreground.
exec ros2 launch rosbridge_server rosbridge_websocket_launch.xml
