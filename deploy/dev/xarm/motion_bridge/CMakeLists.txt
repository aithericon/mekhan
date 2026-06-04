cmake_minimum_required(VERSION 3.8)
project(aithericon_motion_bridge)

if(CMAKE_COMPILER_IS_GNUCXX OR CMAKE_CXX_COMPILER_ID MATCHES "Clang")
  add_compile_options(-Wall -Wextra -Wpedantic)
endif()

find_package(ament_cmake REQUIRED)
find_package(rosidl_default_generators REQUIRED)
find_package(geometry_msgs REQUIRED)
find_package(trajectory_msgs REQUIRED)

# Generate the PlanToPose service. The request references geometry_msgs/Pose and
# the response carries the INNER trajectory_msgs/JointTrajectory as an opaque
# JSON string (declared `string trajectory`), so the only interface DEPENDENCIES
# are geometry_msgs + trajectory_msgs. (Deliberately NOT moveit_msgs: declaring
# the trajectory field as moveit_msgs/RobotTrajectory would trigger the deep
# nested-recursion deriver path for no value -- decision 4.)
rosidl_generate_interfaces(${PROJECT_NAME}
  "srv/PlanToPose.srv"
  DEPENDENCIES geometry_msgs trajectory_msgs
)

# Install the rclpy node so `ros2 run aithericon_motion_bridge motion_bridge_node.py`
# resolves it. install(PROGRAMS ...) preserves the executable bit set in the repo.
install(PROGRAMS
  scripts/motion_bridge_node.py
  DESTINATION lib/${PROJECT_NAME}
)

ament_package()
