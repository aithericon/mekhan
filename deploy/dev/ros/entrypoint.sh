#!/usr/bin/env bash
# Dev turtlesim + rosbridge launcher. turtlesim needs an X display, so we run a
# virtual framebuffer (Xvfb) and point DISPLAY at it; the GUI is never shown
# (an optional VNC sidecar could attach to :1 for humans). rosbridge_websocket
# + rosapi run in the foreground as PID 1's child so the container's lifecycle
# tracks the bridge.
# NOTE: no `set -u` — ROS's setup.bash references unbound vars (AMENT_*).
set -eo pipefail

source /opt/ros/jazzy/setup.bash

# Virtual display for turtlesim's Qt window.
Xvfb :1 -screen 0 1280x1024x24 -nolisten tcp >/tmp/xvfb.log 2>&1 &
export DISPLAY=:1
sleep 1

# The toy robot.
ros2 run turtlesim turtlesim_node >/tmp/turtlesim.log 2>&1 &

# rosbridge WebSocket (port 9090) + rosapi (interface introspection) in the
# foreground. The launch file starts both.
exec ros2 launch rosbridge_server rosbridge_websocket_launch.xml
