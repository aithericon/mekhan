# Isaac Sim xArm6 stack (remote GPU host)

Phase 0 of the sim-first lab plan: the platform's existing xArm6 ROS stack
(`deploy/dev/xarm` — MoveIt + motion_bridge + rosbridge) running with **NVIDIA
Isaac Sim as the physics hardware** instead of ros2_control's fake mock. The
executor ROS backend, all five ROS ops, and the demos (40/49/51/52) are
upstream of the swapped seam and run unchanged.

```
 executor (Mac dev stack) ──ws──▶ rosbridge :9090 ┐
                                                  │ xarm container (Jazzy)
                  move_group ◀── motion_bridge ◀──┘ HW_BACKEND=isaac
                      │ FollowJointTrajectory
                      ▼
            xarm6_traj_controller (ros2_control)
                      │ topic_based_ros2_control/TopicBasedSystem
        /isaac_joint_commands │ ▲ /isaac_joint_states     (DDS domain 42, UDPv4)
                      ▼       │
                  isaac container — PhysX articulation, RTX GPU
```

The hardware seam is **NVIDIA's canonical MoveIt↔Isaac integration**: a
`TopicBasedSystem` ros2_control plugin exchanging `sensor_msgs/JointState`
with the sim. Arm and gripper are two ros2_control systems sharing the one
topic pair; joints are matched by name.

## Files

| File | Role |
|------|------|
| `docker-compose.yml` | `isaac` (Isaac Sim 5.1, GPU 0, headless) + `xarm` (aithericon-xarm:jazzy, `HW_BACKEND=isaac`); host network + host IPC |
| `isaac/isaac_xarm_scene.py` | standalone Isaac script: URDF import → articulation, ROS 2 bridge graph (sub commands / pub states) |
| `prepare-assets.sh` | turns the committed robot-description asset (`demos/assets/files/xarm6.urdf` + mesh zip) into an importer-ready URDF (strips `<ros2_control>`, relativizes `package://` refs) |
| `fastdds-udp-only.xml` | forces Fast DDS onto UDPv4 — same-host containers negotiate shared memory they can't actually share |
| `sync-to-host.sh` | rsync the stack + build context + asset bundle to the GPU host, run asset prep |

## Runbook

```bash
# 1. From repo root (VPN up; key auth to the host):
deploy/sim/isaac/sync-to-host.sh                  # default hydra-2@131.246.221.73

# 2. On the host — first run builds aithericon-xarm:jazzy (several minutes,
#    multi-GB) and needs nvcr.io/nvidia/isaac-sim:5.1.0 pulled (~10 GB; if the
#    pull 401s, `docker login nvcr.io` with $oauthtoken + a free NGC API key):
ssh hydra-2@131.246.221.73 'cd aithericon-sim/deploy/sim/isaac && docker compose up -d --build'

# 3. Verify, in order:
ssh hydra-2@131.246.221.73 '
  docker logs aithericon-isaac 2>&1 | tail -5          # "running: 60 Hz physics …"
  docker exec aithericon-isaac-xarm bash -lc \
    "source /opt/ros/jazzy/setup.bash && timeout 5 ros2 topic echo /isaac_joint_states --once" # sim → ROS
  docker exec aithericon-isaac-xarm bash -lc \
    "source /opt/ros/jazzy/setup.bash && ros2 control list_controllers"  # traj controllers active
'
# noVNC RViz viewer: http://<host>:6080/vnc.html (watch MoveIt's view of the arm)

# 4. Mac side — point a dev ROS runner at the host's rosbridge:
#    EXECUTOR_ROS__WS_URL=ws://131.246.221.73:9090
#    (everything else identical to `just dev xarm-up`'s runner enrollment)
```

First Isaac boot compiles RTX shaders (minutes, looks hung); the cache volumes
make later boots fast.

## Known limits / next

- **Wall-clock vs sim time**: controllers run on wall time; fine while the sim
  holds real-time at 60 Hz (trivial scene on an RTX 3080). If heavy scenes drag
  below real-time, switch to `/clock` + `use_sim_time`.
- **Grasping is scene-attach, not friction**: motion_bridge `/grasp` attaches
  objects in MoveIt's planning scene (as on the fake stack). The sample boxes
  exist only MoveIt-side; mirroring them into Isaac (and eventually contact
  grasping) is Phase 1+ territory.
- **Phase 1 capture**: camera/contact topics from Isaac, `record_topics`
  executor op, stream-tee artifacts — tracked in the plan, not here yet.
