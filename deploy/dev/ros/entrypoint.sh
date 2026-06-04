#!/usr/bin/env bash
# Dev turtlesim + rosbridge launcher. turtlesim needs an X display, so we run a
# virtual framebuffer (Xvfb) and point DISPLAY at it; x11vnc + noVNC then export
# that display so a human can WATCH the turtle in a browser. rosbridge_websocket
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

# Browser viewer: x11vnc exports the Xvfb :1 display over RFB (:5900); websockify
# wraps it as a noVNC web client on :6080 so a human can open
# http://localhost:<host-port>/vnc.html and watch the turtle move. Best-effort —
# failures here never block the rosbridge below. `-bg` daemonizes x11vnc; the
# `|| true` keeps `set -e` from aborting if the viewer can't start.
x11vnc -display :1 -forever -shared -nopw -rfbport 5900 -bg -quiet >/tmp/x11vnc.log 2>&1 || true
websockify --web=/usr/share/novnc 6080 localhost:5900 >/tmp/novnc.log 2>&1 &

# rosapi (interface introspection: /rosapi/topics, /rosapi/message_details, …).
# The Jazzy `rosbridge_websocket_launch.xml` does NOT start rosapi_node, so the
# runner's interface-catalog publish (which introspects entirely over rosapi)
# never resolves unless we launch it ourselves. Run it as a background node so
# `ros2 launch rosbridge_server …` can stay PID 1's foreground child.
ros2 run rosapi rosapi_node >/tmp/rosapi.log 2>&1 &

# rosbridge WebSocket (port 9090) in the foreground.
exec ros2 launch rosbridge_server rosbridge_websocket_launch.xml
