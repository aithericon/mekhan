cmake_minimum_required(VERSION 3.8)
project(aithericon_motion_bridge)

if(CMAKE_COMPILER_IS_GNUCXX OR CMAKE_CXX_COMPILER_ID MATCHES "Clang")
  add_compile_options(-Wall -Wextra -Wpedantic)
endif()

find_package(ament_cmake REQUIRED)
find_package(rosidl_default_generators REQUIRED)
find_package(geometry_msgs REQUIRED)
find_package(trajectory_msgs REQUIRED)
# S2/S3 srv reference moveit_msgs (PoseStamped is geometry_msgs; the scene/grasp
# srv keep their fields flat -- but moveit_msgs + shape_msgs are now interface
# DEPENDENCIES so generation resolves cleanly and downstream introspection works).
find_package(moveit_msgs REQUIRED)
find_package(shape_msgs REQUIRED)

# Generate the bridge services.
#   PlanToPose / PlanCartesian carry the INNER trajectory_msgs/JointTrajectory as
#     an opaque JSON string (declared `string trajectory`), not a moveit_msgs type,
#     so we avoid the deep nested-recursion deriver path (decision 4).
#   AddObject takes a geometry_msgs/PoseStamped; PlanCartesian a geometry_msgs/Pose[].
#   The scene/grasp srv (RemoveObject/ClearScene/Grasp/Release) are flat scalars.
# Interface DEPENDENCIES: geometry_msgs + trajectory_msgs as before, plus
# moveit_msgs + shape_msgs (referenced by the node's scene mutation, declared here
# so the package's interface deps stay accurate).
rosidl_generate_interfaces(${PROJECT_NAME}
  "srv/PlanToPose.srv"
  "srv/AddObject.srv"
  "srv/RemoveObject.srv"
  "srv/ClearScene.srv"
  "srv/PlanCartesian.srv"
  "srv/Grasp.srv"
  "srv/Release.srv"
  DEPENDENCIES geometry_msgs trajectory_msgs moveit_msgs shape_msgs
)

# Install the rclpy node so `ros2 run aithericon_motion_bridge motion_bridge_node.py`
# resolves it. install(PROGRAMS ...) preserves the executable bit set in the repo.
install(PROGRAMS
  scripts/motion_bridge_node.py
  DESTINATION lib/${PROJECT_NAME}
)

ament_package()
