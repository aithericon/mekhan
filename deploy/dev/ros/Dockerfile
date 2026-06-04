# Dev-only turtlesim + rosbridge container for the `just dev ros-up` layer.
# Provides a headless TurtleSim robot reachable over the rosbridge WebSocket
# (+ rosapi for interface introspection), standing in for a real ROS 2 robot
# runner during ROS-backend development. ROS 2 Jazzy (LTS, Ubuntu 24.04).
FROM ros:jazzy-ros-base

# turtlesim (the toy robot), rosbridge_suite (WS bridge + rosapi introspection),
# xvfb (turtlesim is a Qt GUI node and refuses to start without an X display).
# x11vnc + noVNC + websockify give a browser-openable view of the Xvfb display
# so a human can WATCH the turtle move (dev convenience; see entrypoint.sh).
RUN apt-get update && apt-get install -y --no-install-recommends \
        ros-jazzy-turtlesim \
        ros-jazzy-rosbridge-suite \
        xvfb \
        x11vnc \
        novnc \
        python3-websockify \
    && rm -rf /var/lib/apt/lists/*

# rosapi 2.6.0 (the version apt ships for Jazzy at this image's build) has a
# broken `/rosapi/action_type`: its `proxy.get_action_type` imports
# `get_action_names_and_types` from `ros2action.api`, which calls
# `node.get_action_names_and_types()` — a method Jazzy's `rclpy.node.Node` no
# longer provides (it moved to the free function
# `rclpy.action.get_action_names_and_types(node)`). The resulting AttributeError
# is uncaught and takes down the WHOLE rosapi node, killing all interface
# introspection right when the runner's catalog publish reaches actions. Repoint
# the import at the working free function (same signature, `node=` kwarg). This
# unblocks live action typedef introspection (`/rosapi/action_{goal,result,
# feedback}_details`). Harmless no-op if a future base image already ships a
# rosapi that imports it correctly (the `^from ros2action.api import …` line
# simply won't match).
RUN sed -i \
        's/^from ros2action.api import get_action_names_and_types/from rclpy.action import get_action_names_and_types/' \
        /opt/ros/jazzy/lib/python3.12/site-packages/rosapi/proxy.py

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# rosbridge WebSocket (rosapi rides the same port via the launch file) + the
# noVNC web viewer (browser → :6080/vnc.html → the turtle's Xvfb display).
EXPOSE 9090 6080

ENTRYPOINT ["/entrypoint.sh"]
