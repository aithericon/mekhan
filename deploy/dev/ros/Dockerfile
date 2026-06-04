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

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# rosbridge WebSocket (rosapi rides the same port via the launch file) + the
# noVNC web viewer (browser → :6080/vnc.html → the turtle's Xvfb display).
EXPOSE 9090 6080

ENTRYPOINT ["/entrypoint.sh"]
