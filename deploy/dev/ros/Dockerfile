# Dev-only turtlesim + rosbridge container for the `just dev ros-up` layer.
# Provides a headless TurtleSim robot reachable over the rosbridge WebSocket
# (+ rosapi for interface introspection), standing in for a real ROS 2 robot
# runner during ROS-backend development. ROS 2 Jazzy (LTS, Ubuntu 24.04).
FROM ros:jazzy-ros-base

# turtlesim (the toy robot), rosbridge_suite (WS bridge + rosapi introspection),
# xvfb (turtlesim is a Qt GUI node and refuses to start without an X display).
RUN apt-get update && apt-get install -y --no-install-recommends \
        ros-jazzy-turtlesim \
        ros-jazzy-rosbridge-suite \
        xvfb \
    && rm -rf /var/lib/apt/lists/*

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# rosbridge WebSocket (rosapi rides the same port via the launch file).
EXPOSE 9090

ENTRYPOINT ["/entrypoint.sh"]
