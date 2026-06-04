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

# OPTIONAL browser viewer (mirrors the turtlesim harness): launch RViz with the
# xArm MoveIt config under the Xvfb display, export it over RFB with x11vnc, and
# wrap it as a noVNC web client so a human can open
# http://localhost:<host-port>/vnc.html and WATCH the 3D arm execute the demo
# trajectories. Unlike turtlesim (a 2D Qt window), RViz is an OpenGL app, so the
# container needs a software GL stack — `libgl1-mesa-dri` provides the llvmpipe
# renderer and `LIBGL_ALWAYS_SOFTWARE=1` (below) forces it (there's no GPU in
# the container). `fluxbox` is a tiny window manager so RViz maps/maximizes into
# a usable desktop instead of a bare floating window. rviz2 + the MoveIt
# MotionPlanning RViz plugin are pulled explicitly (they normally arrive as
# moveit deps, but pin them so `show_rviz:=true` can't fail on a missing plugin).
# This is its own thin layer AFTER the heavy colcon build so toggling the viewer
# never busts the multi-GB build cache.
RUN apt-get update && apt-get install -y --no-install-recommends \
        x11vnc \
        novnc \
        python3-websockify \
        fluxbox \
        xdotool \
        libgl1-mesa-dri \
        ros-jazzy-rviz2 \
        ros-jazzy-moveit-ros-visualization \
    && rm -rf /var/lib/apt/lists/*

# Force software OpenGL (llvmpipe) — there is no GPU in the container, so RViz
# must not try a hardware GL context (it would fail to create a window).
ENV LIBGL_ALWAYS_SOFTWARE=1

# fluxbox "remember" rule: maximize any rviz2-class window the moment it maps,
# and keep it maximized. This is the reliable, WM-native way to fill the
# viewport — far better than racing RViz's slow software-GL layout with an
# xdotool resize loop (RViz also spawns tiny helper windows — a 1x1 "rviz2" and
# a "Qt Selection Owner" — that confuse title-based matching). The entrypoint
# additionally nudges it via xdotool as a backup.
RUN mkdir -p /root/.fluxbox \
    && printf '[app] (class=rviz2)\n  [Maximized]\t{yes}\n[end]\n' > /root/.fluxbox/apps

# Isolate this container's ROS graph on a non-default DDS domain. Docker
# Desktop's bridge passes DDS multicast between containers, so a default-domain
# (0) xArm would cross-discover a default-domain turtlesim container running in
# the same checkout — polluting the runner's interface catalog (rosapi would
# introspect the turtle's topics too). A distinct domain confines rosapi/rosbridge
# to this container's own nodes. (The turtle harness stays on domain 0.)
ENV ROS_DOMAIN_ID=42

# ── Path C motion-bridge: adds /plan_to_pose returning the planned trajectory
#    (stock xarm_pose_plan returns only {success}). A plain rclpy node calling
#    move_group's own /compute_ik + /plan_kinematic_path, returning the inner
#    JointTrajectory as JSON for the platform to splice into Path B. Own thin
#    layer placed AFTER the heavy xarm_ros2/MoveIt colcon build + the rosapi
#    seds, so editing the bridge never busts the multi-GB cache. Source the
#    just-built workspace overlay (/ros2_ws/install) so the new package's
#    interface generation resolves moveit_msgs/geometry_msgs/trajectory_msgs.
COPY motion_bridge /ros2_ws/src/aithericon_motion_bridge
RUN . /opt/ros/jazzy/setup.sh \
    && . /ros2_ws/install/setup.sh \
    && colcon build --packages-select aithericon_motion_bridge

# ── Enable the gripper in the fake sim. xarm6_planner_fake.launch.py hard-codes
#    its launch_arguments and does NOT forward add_gripper, so the stock fake arm
#    has no gripper: drive_joint never appears in /joint_states and the
#    xarm_gripper_traj_controller action server never actuates. Patch the wrapper
#    (installed copy is what `ros2 launch` loads; src patched too for parity) to
#    pass add_gripper:=true, so move_group + fake ros2_control bring up the
#    UFACTORY gripper — drive_joint is published, the gripper links render in RViz,
#    and the motion-bridge /grasp,/release ops drive a REAL, visible gripper
#    close/open on top of the scene attach. Additive: the separately-launched
#    xarm_planner_node (demo 30 services) and the arm "xarm6" planning group are
#    untouched. The `\1`-anchored sed is a harmless no-op if a future wrapper
#    already forwards the arg.
RUN sed -i "s/\(['\"]no_gui_ctrl['\"]: ['\"]true['\"],\)/\1\n            'add_gripper': 'true',/" \
        /ros2_ws/install/xarm_planner/share/xarm_planner/launch/xarm6_planner_fake.launch.py \
        /ros2_ws/src/xarm_ros2/xarm_planner/launch/xarm6_planner_fake.launch.py

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# rosbridge WebSocket (rosapi rides the same port via the launch file) + the
# noVNC web viewer (browser → :6080/vnc.html → RViz on the Xvfb display).
EXPOSE 9090 6080

ENTRYPOINT ["/entrypoint.sh"]
