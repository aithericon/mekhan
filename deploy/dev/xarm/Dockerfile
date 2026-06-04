# Dev-only headless xArm 6 (fake hardware) + rosbridge container for the
# `just dev xarm-up` layer. Brings up a SIMULATED UFACTORY xArm 6 reachable over
# the rosbridge WebSocket (+ rosapi introspection), standing in for the real arm
# during ROS-backend development. ROS 2 Jazzy (LTS, Ubuntu 24.04).
#
# Unlike the turtlesim harness (apt-installable), xarm_ros2 ships ONLY as source,
# so this image clones the `jazzy` branch and colcon-builds the planner stack
# (xarm_planner + xarm_moveit_config + ros2_control fake hardware). That makes
# this a HEAVY image — MoveIt pulls multi-GB of deps and the first build is
# several minutes (cached after). Gazebo / vision / servo packages are pruned
# before the build since fake mode needs none of them.
#
# Sim → hardware: the entrypoint launches `xarm6_planner_fake.launch.py`; the
# real arm is the SAME stack via `xarm6_planner_realmove.launch.py robot_ip:=…`.
FROM ros:jazzy-ros-base

# Build toolchain (colcon + rosdep) + rosbridge_suite (WS bridge + rosapi).
# MoveIt / ros2_control / controllers are resolved by rosdep from the package
# manifests. xvfb is a cheap safety net: we launch with `show_rviz:=false`, but
# if any GL/Qt node still probes for a display, a virtual framebuffer keeps the
# container from hard-failing at boot (mirrors the turtlesim harness).
RUN apt-get update && apt-get install -y --no-install-recommends \
        git \
        build-essential \
        python3-colcon-common-extensions \
        python3-rosdep \
        ros-jazzy-rosbridge-suite \
        xvfb \
    && rm -rf /var/lib/apt/lists/*

# Clone xarm_ros2 (jazzy) into a colcon workspace, recursive for the C++ SDK
# submodule. Prune the packages fake mode doesn't need so rosdep never pulls
# their heavy deps (Gazebo sim, RealSense, MoveIt servo).
WORKDIR /ros2_ws/src
RUN git clone https://github.com/xArm-Developer/xarm_ros2.git --recursive -b jazzy \
    && rm -rf \
        xarm_ros2/xarm_gazebo \
        xarm_ros2/xarm_vision \
        xarm_ros2/xarm_moveit_servo

# Resolve deps + build only the planner stack (xarm_planner and everything it
# depends on: xarm_moveit_config, xarm_controller, xarm_description, xarm_msgs,
# uf_ros_lib). `--packages-up-to xarm_planner` skips xarm_api (SDK-mirror
# services that need a live controller connection — useless in fake mode).
WORKDIR /ros2_ws
# `--skip-keys` for the pruned packages: kept manifests (e.g. xarm_moveit_config)
# still declare exec-deps on xarm_gazebo etc., so rosdep would otherwise fail on
# the now-dangling keys. They're runtime deps fake mode never loads, so skipping
# is safe.
RUN apt-get update \
    && rosdep update \
    && rosdep install --from-paths src --ignore-src --rosdistro jazzy -y \
        --skip-keys "xarm_gazebo xarm_vision xarm_moveit_servo" \
    && rm -rf /var/lib/apt/lists/*
RUN . /opt/ros/jazzy/setup.sh && colcon build --packages-up-to xarm_planner

# Same rosapi 2.6.0 `/rosapi/action_type` fix as the turtlesim harness: repoint
# the broken `ros2action.api` import at the working `rclpy.action` free function
# so live action introspection doesn't crash the rosapi node. (See
# deploy/dev/ros/Dockerfile for the full root-cause write-up.)
RUN sed -i \
        's/^from ros2action.api import get_action_names_and_types/from rclpy.action import get_action_names_and_types/' \
        /opt/ros/jazzy/lib/python3.12/site-packages/rosapi/proxy.py

# Two MORE rosapi 2.6.0 typedef-introspection crashes the xArm's rich message
# types trigger but the flat turtlesim types never did. ONE uncaught exception in
# objectutils kills the whole rosapi node, so the runner's full catalog sweep
# (201 interfaces) dies mid-way → no catalog. Both are upstream bugs in
# `objectutils.py`; patch them to degrade gracefully (harmless no-ops if a future
# rosapi already fixed the lines — the `^…$`-anchored seds simply won't match):
#
#  1. `_type_name` asserts `isinstance(instance, ROSMessage)` for every non-list,
#     non-atomic field value — but rosidl represents numeric sequences as
#     `array.array`, so any message carrying one (e.g. covariance fields) hits the
#     assert. Fall back to the already-correct declared `type_name` instead.
#  2. `_handle_type_and_array_len` parses a BOUNDED sequence `sequence<T, N>`
#     (`.msg` `T[<=N]`, common in moveit_msgs / rcl_interfaces) with a regex that
#     captures `"T, N"` as the element type; that bogus string is later loaded as
#     a message class → InvalidTypeStringException. Keep only the element type `T`.
RUN OBJ=/opt/ros/jazzy/lib/python3.12/site-packages/rosapi/objectutils.py \
    && sed -i \
        's/^    assert isinstance(instance, ROSMessage)$/    if not isinstance(instance, ROSMessage):\n        return type_name/' \
        "$OBJ" \
    && sed -i \
        's/^        field_type = matches\[0\]$/        field_type = matches[0].split(",")[0].strip()/' \
        "$OBJ"

# Isolate this container's ROS graph on a non-default DDS domain. Docker
# Desktop's bridge passes DDS multicast between containers, so a default-domain
# (0) xArm would cross-discover a default-domain turtlesim container running in
# the same checkout — polluting the runner's interface catalog (rosapi would
# introspect the turtle's topics too). A distinct domain confines rosapi/rosbridge
# to this container's own nodes. (The turtle harness stays on domain 0.)
ENV ROS_DOMAIN_ID=42

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# rosbridge WebSocket (rosapi rides the same port via the launch file).
EXPOSE 9090

ENTRYPOINT ["/entrypoint.sh"]
