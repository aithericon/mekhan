# 27 — MoveIt Motion-Planning: Implementation Plan (Path C)

This is the build spec for the motion-planning feature whose **design rationale and
six load-bearing decisions live in [`docs/26-motion-planning.md`](26-motion-planning.md)** — do
not relitigate those here. The north star is physics-lab sample handling (pick from rack → place
into instrument → swap). The reframe from docs/26 is the spine of this plan: **Path C adds a
"plan" primitive** (a MoveIt plan emitting an opaque trajectory blob + a typed metrics sidecar);
**execution reuses the already-shipped Path B** (`FollowJointTrajectory` action); and **pick /
place / swap are SubWorkflows over those primitives, not new backend operations.** The headline
result of the six parallel codebase investigations is that **almost no platform code is new** — the
entire feature is delivered through (a) a runner-side ROS "motion-bridge" node on the xArm image
exposing semantic services, (b) demo fixtures (graph.json / demo.json / assets), and (c) the
existing `call_service` + `send_action_goal` ROS backend operations, the existing Decision/SubWorkflow/
LeaseScope/Map node types, and the existing asset layer. The plan is sliced S1→S4 exactly as docs/26
specifies, each slice independently LIVE-GREEN-able.

---

## Ground truth (confirmed by the investigators against real files)

These are the facts a future implementer can trust. Each was verified against the live tree (the
investigation ran in the `xarm-pathb` checkout; this plan and docs/26 now live together on
`feat/motion-planning`, slot 5).

### The ROS backend already does everything the bridge needs — ZERO backend code
- **`call_service` sends NO type on the wire.** `executor/crates/executor-ros/src/client.rs:268`
  emits `{op:"call_service", service, id, args}` only; `backend.rs` `validate_static` (≈line 460)
  explicitly allows an empty `interface_type` for `CallService`. rosbridge resolves the `.srv` type
  server-side from the live ROS graph, so **any custom non-`std_srvs` service works the moment its
  package is built + sourced on the runner** — already live-proven by demo 30 against
  `xarm_msgs/srv/PlanJoint`.
- **The request body is passed verbatim.** `backend.rs` sends the rendered `fields` JSON object
  directly as rosbridge `args`. `render_value` (`backend.rs:484`) only Tera-renders string leaves
  and recurses into nested objects/arrays — it never reshapes structure. A nested pose object, or a
  spliced trajectory blob, reaches rosbridge unchanged (decision 2).
- **`call_service` responses land in `outputs`; the response object's keys are promoted to
  top-level node outputs** (`promote_object_fields`, `backend.rs:432`). So a `plan_to_pose` service
  returning `{trajectory, planning_time, point_count, success, error_code}` surfaces each key as a
  referenceable node output (`{{plan.trajectory}}`, `{{plan.point_count}}`, …). **By contrast,
  `send_action_goal`'s terminal RESULT rides `stdout_tail` only** (feedback chunks ride outputs) —
  this asymmetry is exactly why decision 2 works: plan (call_service) returns the trajectory as a
  referenceable output, execute (action) just consumes it. **Never map `execute.error_code` in an
  End** — it is not a referenceable output.
- **Whole-object splice through a string-leaf ref is supported** (`backend.rs:496-501`,
  `is_pure_placeholder`): when a `fields` leaf is a *bare* `"{{ plan.trajectory }}"` (a single
  placeholder, no surrounding text), the backend re-parses the rendered output as JSON and adopts
  the typed value if it is not a string. Producer envelope values enter the Tera context as
  `serde_json::Value` (`executor-backend/src/context.rs:36`), and Tera renders a JSON object via
  serde's Display (valid JSON) — so the re-parse restores the object. **This is the linchpin of the
  plan→execute handoff and is the #1 explicit S1 gate** (existing tests cover scalar/bool/string
  coercion at `backend.rs:790-818` but none exercise a whole *object* — verify it live, see S1).

### The deriver shapes a clean output port from a flat-scalar + string response — ZERO deriver code
- `service/src/backends/ros.rs` `derive_output_port` derives a `CallService` port from the
  `{base}_Response` root key, **preferring config-embedded `interface_typedefs` over bundled
  snapshots** (so any robot's runner-reported services generalize). `service/src/backends/ros/typedef.rs`
  `typedefs_to_port` maps each top-level response field: a non-array primitive → a clean scalar
  `FieldKind` with `schema=None` (`double/float*`+ints → Number, `bool/boolean` → Bool,
  `string` → Text, `time/duration` → Json); arrays / nested `pkg/Type` messages → `Json` + a JSON-Schema
  override. **Numeric leaves *inside* schema overrides are emitted nullable `["number","null"]`** to
  survive rosbridge's NaN/±Inf→`null` rendering; **top-level scalar metrics are NOT auto-nullable** —
  the bridge must emit finite numbers (e.g. `0` on failure, never NaN).
- `interface_typedefs` is **editor/deriver-only metadata stripped from the canonical spec.config**
  the executor receives (the compiler round-trips through `RosConfig` which has no such field). The
  runner self-reports it via `executor/crates/executor-service/src/ros_catalog.rs`
  (`/rosapi/service_request_details` + `/rosapi/service_response_details`, skipping `INFRA_PACKAGES`)
  — a custom `aithericon_motion_bridge` package is picked up automatically.

### move_group is ALREADY running in the fake sim; the trajectory-returning service is the only gap
- `xarm6_planner_fake.launch.py` → `_robot_moveit_fake.launch.py` brings up the full MoveIt
  `move_group`: `/plan_kinematic_path` (`moveit_msgs/srv/GetMotionPlan`), `/compute_ik`
  (`GetPositionIK`), `/compute_cartesian_path`, `/get_planning_scene`, `/apply_planning_scene`,
  `/query_planner_interface` are all live. SRDF planning group is **`xarm6`** (also `xarm_gripper`,
  group_states home/hold-up/open/close for later slices).
- **`moveit_py` is NOT installed and `ros-jazzy-moveit-py` is not apt-available** — so the C++
  `constructGoalConstraints` Python helper is OUT. The realistic bridge is a **`rclpy` node that
  calls move_group's own services**.
- **Stock `xarm_pose_plan` returns only `{success}`, NOT the trajectory** (it stashes the plan
  internally for `xarm_exec_plan`). This confirms decision 3's claim: the bridge must add a service
  that *returns* the `RobotTrajectory`.

### The composition primitives already exist — ZERO new node types for any slice
- **Decision** nodes (`demos/03-decision-routing/graph.json`) carry compiler-checked Rhai guards:
  `service/src/compiler/validate.rs` `validate_guards` resolves every `<slug>.<field>` ref against
  the upstream producer's declared output port and synthesizes a non-consuming read-arc
  (`compile.rs` `apply_control_data_foundation`); an unresolvable ref is a hard pre-publish
  `CompileError`. `defaultBranch` must be exactly `"default"` (`DEFAULT_BRANCH_HANDLE_ID`,
  `template.rs:2435`).
- **SubWorkflow** nodes (`template.rs:813`, `service/src/compiler/subworkflow.rs`,
  `lower/subworkflow.rs`): input contract = child `Start.initial`, output contract = union of child
  `End.resultMapping` target fields; `inputMapping` (Rhai over the inbound token) shapes the child
  token, `output` projects the child result back. Sequential call/return; a parked producer
  borrowable downstream as `<slug>.<field>`. **Nested SubWorkflows are supported.**
- **LeaseScope** (`template.rs:484`): any step inside runs warm on one held allocation by
  containment — the natural home for scene single-writer exclusivity (decision 1).
- **Asset layer** (`docs/20-resources-and-assets.md`, `service/src/models/asset.rs`,
  `petri/asset_resolver.rs`, `compiler/asset_refs.rs`): user-typed JSONB record collections,
  scope-resolved, version-pinned. Nested poses/arrays ride `FieldKind::Json` + a `schema` override
  (proven by demos 30/32 `position`/`trajectory` ports). Two consumption modes: node-level
  `assetBindings:[{alias,refKey}]` stages the whole collection as `<alias>.json`; for an
  `object`-cardinality asset, scalar/Json fields are compile-time-constant refs `<ref_key>.<field>`.
  **Collection field projection `<ref_key>[*].<field>` is DEFERRED** — collection row lookups must
  happen in-code (a staged python step) or via a `Map`.

### Fixtures & harness
- demos 30/31/32 are pure on-disk fixtures (`demo.json` + `graph.json` + `tests/*.json`) seeded by
  mekhan-service when `MEKHAN__DEMOS__SEED=true` (set by `just dev`). templateId convention
  `00000000-0000-0000-0000-0000000003N0`: 30=…0300, 31=…0310, 32=…0320 → **next free = …0330**.
- Every ROS step pins to the runner via **both** `deploymentModel:{mode:executor,
  capacity:{alias:"xarm_fleet"}}` **and** `requirements:{constraints:[{capability:"ros",
  field:"robot_model", op:"eq", value:"xarm6"}]}`. `demos/resources/xarm_fleet.json`
  (`config.preset=instrument`) and `demos/capability-types/ros.json` are shared & already correct —
  **no change for any slice.**
- Test assertions key off **`result.value.<field>`** (the End resultMapping), never `steps.*`
  (the step-execution projection lags instance completion and is racy).

### What is genuinely NEW (the entire surface area of this feature)
1. A runner-side ROS package `deploy/dev/xarm/motion_bridge/` (`ament_cmake`, generated `.srv` +
   `rclpy` node) — grows one service per slice.
2. Two small diffs to `deploy/dev/xarm/{Dockerfile,entrypoint.sh}`.
3. Demo fixtures (graph.json/demo.json/tests) per slice, and asset fixtures (S4).
4. **No** Rust changes to mekhan-service or the executor in S1–S3. **Possibly** an asset *fixture*
   (data, not code) in S4 — see the OpenAPI/regen note.

---

## Slice S1 — Pose plan → gate → execute (minimal bridge, one service, no scene/grasp/assets)

**Goal:** drive the simulated xArm 6 through a full MoveIt plan-to-Cartesian-pose, gate the plan on
its typed metrics, and execute the planned trajectory via the shipped Path B
`FollowJointTrajectory`. Prove the opaque-blob handoff and the compiler-checked gate end-to-end.
This slice is implementable **verbatim** from what follows.

### Decision: the bridge node design (resolving investigator contradictions)

Investigators 1, 2, 3 agreed on the architecture; investigator 3 alone went deep on the ROS
internals and surfaced two contradictions to resolve. **Resolutions, decided:**

1. **Package type: `ament_cmake`, NOT `ament_python`.** Investigator 3 correctly caught that a
   custom `.srv` cannot be generated from a pure `ament_python` package (`rosidl_generate_interfaces`
   requires `ament_cmake`). We use **one `ament_cmake` package** that both generates the interface
   AND installs the `rclpy` node via `install(PROGRAMS …)`. (Chosen over investigator 3's earlier
   ament_python sketch.)

2. **Blob shape: the bridge returns the INNER `trajectory_msgs/JointTrajectory` as the opaque
   JSON, NOT the full `moveit_msgs/RobotTrajectory`.** `GetMotionPlan` returns a `RobotTrajectory`
   (which *wraps* `joint_trajectory` + `multi_dof_joint_trajectory`), but Path B's
   `FollowJointTrajectory` goal `.trajectory` is a `JointTrajectory`. Returning the inner
   `joint_trajectory` lets the demo splice `{{plan.trajectory}}` straight into the action goal with
   **zero reshaping**. (This resolves investigators 1/3/4's open question; investigator 3 explicitly
   recommended the inner form — we adopt it.)

3. **Goal construction: IK → joint-goal, not a pose-goal Constraint.** The node calls `/compute_ik`
   to turn the target Pose into joint values, then `/plan_kinematic_path` with hand-built
   `JointConstraint`s — avoiding the unavailable C++ `constructGoalConstraints`. This is the minimal
   robust S1 path; a pose-goal Constraint is a later refinement.

4. **Trajectory output port kind: opaque `json`.** A fully-typed `RobotTrajectory` port is a
   deriver-only upgrade and is explicitly OUT of S1 (decision 2). The `trajectory` output field is
   declared `kind:"json"`; the metrics fields are scalar (`success`→bool, `point_count`/
   `planning_time`/`total_duration`→number, `error_code`→number, `error_message`→text). Declaring
   the *bridge srv* field as `trajectory_msgs/JointTrajectory` (nested) lets rosapi/the deriver type
   it; the demo's output port simply marks it `kind:"json"` (opaque). **Do NOT declare it as
   `moveit_msgs/RobotTrajectory`** in the srv — that triggers the deep nested-recursion path for no
   value (investigator 2's risk).

5. **The "gate" is a real Decision node, not a bare sequence edge.** Investigator 4 proposed a
   plain `sequence` edge ("the split is the gate"); investigator 5 proposed a compiler-checked
   `Decision` on the typed metrics. **We choose the Decision node** — it is the only primitive that
   (a) is compiler-checked against the sidecar field names (a typo fails pre-publish) and (b)
   actually *prevents* execute from firing on a bad plan. It costs nothing extra and is the honest
   reading of docs/26's "plan→gate→execute". No human-task node in S1 (deferred).

6. **Service naming: flat `/plan_to_pose`** for S1 (matches stock `xarm_*` flat naming and rosapi
   discovery). Later slices add sibling services `/add_object`, `/grasp`, etc. (Investigators split
   between `/motion_bridge/plan_to_pose` and flat `/plan_to_pose`; flat is simpler and consistent
   with the existing `/xarm_joint_plan` flat convention. Package name remains
   `aithericon_motion_bridge` so `output_root_key` resolves `aithericon_motion_bridge/PlanToPose_Response`.)

### NEW files

```
deploy/dev/xarm/motion_bridge/package.xml
deploy/dev/xarm/motion_bridge/CMakeLists.txt
deploy/dev/xarm/motion_bridge/srv/PlanToPose.srv
deploy/dev/xarm/motion_bridge/scripts/motion_bridge_node.py   (chmod +x)
demos/33-xarm-pose-plan-execute/demo.json
demos/33-xarm-pose-plan-execute/graph.json
demos/33-xarm-pose-plan-execute/tests/plans-gates-executes.json
```

**`deploy/dev/xarm/motion_bridge/package.xml`** — `ament_cmake`; `build_depend
rosidl_default_generators`; `member_of_group rosidl_interface_packages`; runtime depends `rclpy`,
`moveit_msgs`, `geometry_msgs`, `trajectory_msgs`, `std_msgs`, `rosidl_default_runtime`.

**`deploy/dev/xarm/motion_bridge/CMakeLists.txt`**:
```cmake
cmake_minimum_required(VERSION 3.8)
project(aithericon_motion_bridge)
find_package(ament_cmake REQUIRED)
find_package(rosidl_default_generators REQUIRED)
find_package(geometry_msgs REQUIRED)
find_package(trajectory_msgs REQUIRED)
rosidl_generate_interfaces(${PROJECT_NAME}
  "srv/PlanToPose.srv"
  DEPENDENCIES geometry_msgs trajectory_msgs)
install(PROGRAMS scripts/motion_bridge_node.py DESTINATION lib/${PROJECT_NAME})
ament_package()
```

**`deploy/dev/xarm/motion_bridge/srv/PlanToPose.srv`**:
```
# --- request ---
geometry_msgs/Pose target            # goal end-effector pose
string  group                        # planning group; empty -> node default "xarm6"
string  frame_id                     # planning frame; empty -> node default "link_base"
float64 allowed_planning_time        # 0 -> node default (5.0)
int32   num_planning_attempts        # 0 -> node default (10)
---
# --- response: opaque inner JointTrajectory + typed metrics sidecar ---
bool    success
string  trajectory                   # opaque trajectory_msgs/JointTrajectory as JSON (spliced verbatim into Path B)
float64 planning_time                # seconds, from MotionPlanResponse
int32   point_count                  # len(trajectory.points)
float64 total_duration               # last point time_from_start, seconds
int32   error_code                   # MoveItErrorCode.val (1 == SUCCESS)
string  error_message                # human-readable on failure
```
All response fields are flat scalars + one string → trivial, crash-free rosapi typedefs.

**`deploy/dev/xarm/motion_bridge/scripts/motion_bridge_node.py`** — `rclpy` node, **MUST** use
`MultiThreadedExecutor` + `ReentrantCallbackGroup` so the `/plan_to_pose` callback can synchronously
`.call()` the move_group clients without deadlock. Algorithm:
1. `/compute_ik` (`GetPositionIK`): `PositionIKRequest{group_name=group, avoid_collisions=true,
   pose_stamped=PoseStamped(header.frame_id=frame, pose=req.target)}`. On `error_code.val != 1`,
   return `success=false, error_code=<val>, error_message="IK failed", trajectory="", point_count=0,
   planning_time=0.0, total_duration=0.0` (finite numbers — never NaN).
2. `/plan_kinematic_path` (`GetMotionPlan`): `MotionPlanRequest{group_name=group,
   num_planning_attempts=req.num_planning_attempts or 10, allowed_planning_time=req.allowed_planning_time
   or 5.0, goal_constraints=[Constraints(joint_constraints=[JointConstraint(joint_name=n, position=p,
   tolerance_above=1e-3, tolerance_below=1e-3, weight=1.0) for n,p in zip(ik.solution.joint_state.name,
   .position) if n in ARM_JOINTS])]}`. (`ARM_JOINTS` = the 6 xarm joints; filter the gripper.)
3. On success serialize the **inner** `motion_plan_response.trajectory.joint_trajectory` via
   `rosidl_runtime_py.convert.message_to_ordereddict` → `json.dumps` → `resp.trajectory`. Fill
   `planning_time`, `point_count=len(points)`, `total_duration=(pts[-1].time_from_start.sec +
   nanosec/1e9) if pts else 0.0`, `error_code=1`, `success=true`.
4. `main()`: `rclpy.init()`; `MultiThreadedExecutor().add_node(MotionBridge()).spin()`. Node
   `create_client`-waits, so launch order vs move_group is forgiving.

### EDITS to existing files

**`deploy/dev/xarm/Dockerfile`** — add as its OWN thin layer AFTER the heavy `xarm_ros2`/MoveIt
build (so editing the bridge never busts the multi-GB cache), and BEFORE the existing rosapi seds
remain in place (they patch site-packages; order vs this build is irrelevant — leave them where
they are):
```dockerfile
# ── Path C motion-bridge: adds /plan_to_pose returning the planned trajectory
#    (stock xarm_pose_plan returns only {success}). Own layer to protect the cache.
COPY motion_bridge /ros2_ws/src/aithericon_motion_bridge
RUN . /opt/ros/jazzy/setup.sh \
    && . /ros2_ws/install/setup.sh \
    && colcon build --packages-select aithericon_motion_bridge
```

**`deploy/dev/xarm/entrypoint.sh`** — add after the existing planner/move_group launch block (the
node self-waits for `/compute_ik` + `/plan_kinematic_path`, so timing is best-effort; do NOT use
`&` inside a `docker exec` — but the entrypoint backgrounds via the shell directly, which is fine):
```bash
# Path C motion-bridge (re-exposes move_group planning as /plan_to_pose).
ros2 run aithericon_motion_bridge motion_bridge_node.py >/tmp/motion_bridge.log 2>&1 &
```

**No edit to `just/dev.just`.** But the existing `aithericon-xarm:jazzy` image **must be rebuilt**
(the `xarm-up` recipe only builds when the image is ABSENT). Either `docker rmi aithericon-xarm:jazzy`
then `just dev xarm-up`, or `docker build -t aithericon-xarm:jazzy deploy/dev/xarm` explicitly.
This is the single most-forgotten step — if `/plan_to_pose` "doesn't appear", the image is stale.

### Demo wiring (`demos/33-xarm-pose-plan-execute/`)

Net: `start → plan_pose (call_service /plan_to_pose) → gate (Decision on metrics) → [accept]
execute (send_action_goal FollowJointTrajectory) → read_state (await_topic /joint_states) → end-done`;
`[default] → end-rejected`.

**`demo.json`**:
```json
{
  "templateId": "00000000-0000-0000-0000-000000000330",
  "name": "33 · xArm Pose Plan → Execute (MoveIt, Path C Slice 1)",
  "description": "Path C Slice 1 on the SIMULATED xArm 6: a MoveIt motion-plan to a Cartesian pose, GATED on the typed metrics sidecar, then EXECUTED via the shipped Path B FollowJointTrajectory. start → plan_pose (call_service /plan_to_pose — the runner-side motion-bridge node calling MoveIt's own /compute_ik + /plan_kinematic_path and RETURNING the planned trajectory the stock xarm_pose_plan does not) → gate (Decision: success && point_count>0 && total_duration<=30) → execute (send_action_goal /xarm6_traj_controller/follow_joint_trajectory, the planned trajectory spliced verbatim as {{ plan_pose.trajectory }}) → read_state (await_topic /joint_states) → End. LIVE ONLY: requires `just dev xarm-up`."
}
```

**`graph.json`** (mirror demo 30 placement; key blocks):
- `start` — empty initial fields, `processName:"xArm Pose Plan → Execute"`.
- `plan_pose` (slug `plan_pose`, `automated_step`) — same `deploymentModel` + `requirements` as
  demo 30; executionSpec:
  ```json
  { "backendType": "ros", "config": {
    "operation": "call_service",
    "interface_name": "/plan_to_pose",
    "interface_type": "aithericon_motion_bridge/srv/PlanToPose",
    "fields": {
      "target": { "position": {"x":0.3,"y":0.0,"z":0.4},
                  "orientation": {"x":0.0,"y":0.0,"z":0.0,"w":1.0} },
      "group": "xarm6", "frame_id": "link_base",
      "allowed_planning_time": 5.0, "num_planning_attempts": 10 },
    "interface_typedefs": [
      {"type":"aithericon_motion_bridge/PlanToPose_Request",
       "fieldnames":["target","group","frame_id","allowed_planning_time","num_planning_attempts"],
       "fieldtypes":["geometry_msgs/Pose","string","string","double","int32"],
       "fieldarraylen":[-1,-1,-1,-1,-1]},
      {"type":"aithericon_motion_bridge/PlanToPose_Response",
       "fieldnames":["success","trajectory","planning_time","point_count","total_duration","error_code","error_message"],
       "fieldtypes":["boolean","string","double","int32","double","int32","string"],
       "fieldarraylen":[-1,-1,-1,-1,-1,-1,-1]}
    ] } }
  ```
  output port (declare opaque trajectory + scalar metrics):
  ```json
  { "id":"out","label":"Output","fields":[
    {"name":"trajectory","label":"Trajectory","kind":"json","schema":{"type":"object"}},
    {"name":"success","label":"Success","kind":"bool"},
    {"name":"planning_time","label":"Planning time (s)","kind":"number"},
    {"name":"point_count","label":"Point count","kind":"number"},
    {"name":"total_duration","label":"Total duration (s)","kind":"number"} ] }
  ```
  > **Target-pose caveat:** `{x:0.3,y:0.0,z:0.4}` is a starting guess — IK must succeed for the
  > pose in `link_base` frame, else S1 looks broken when it is a bad target. Pick a pose near the
  > `home`/`hold-up` SRDF group_state region; tune live in the first run (see S1 risks).

- `gate` (slug `gate`, `decision`):
  ```json
  { "type":"decision","label":"Plan acceptable?",
    "conditions":[{"edgeId":"branch-accept","label":"Accept",
      "guard":"plan_pose.success == true && plan_pose.point_count > 0 && plan_pose.total_duration <= 30.0"}],
    "defaultBranch":"default" }
  ```
- `execute` (slug `execute`, `automated_step`) — Path B, splice the inner trajectory verbatim:
  ```json
  { "backendType":"ros","config":{
    "operation":"send_action_goal",
    "interface_name":"/xarm6_traj_controller/follow_joint_trajectory",
    "interface_type":"control_msgs/action/FollowJointTrajectory",
    "fields":{ "trajectory": "{{ plan_pose.trajectory }}" },
    "interface_typedefs":[{"type":"control_msgs/FollowJointTrajectory_Result",
      "fieldnames":["error_code","error_string"],"fieldtypes":["int32","string"],"fieldarraylen":[-1,-1]}] } }
  ```
  output: `[{"name":"feedback_count","kind":"number"}]` — **RESULT error_code rides stdout_tail; do
  NOT map it.** `"trajectory"` MUST be the bare string `"{{ plan_pose.trajectory }}"` (whole-object
  splice depends on `is_pure_placeholder`).
- `read_state` (slug `read_state`) — copy demo 30/31 `await_topic /joint_states` node VERBATIM
  (JointState typedefs + the 5 json output fields with nullable-number array schemas).
- `end-done` (`end`) — resultMapping:
  ```json
  [ {"targetField":"planned","expression":"true"},
    {"targetField":"plan_ok","expression":"plan_pose.success"},
    {"targetField":"planning_time","expression":"plan_pose.planning_time"},
    {"targetField":"point_count","expression":"plan_pose.point_count"},
    {"targetField":"joint_names","expression":"read_state.name"},
    {"targetField":"positions","expression":"read_state.position"} ]
  ```
- `end-rejected` (`end`) — resultMapping: `[{"targetField":"planned","expression":"false"},
  {"targetField":"reason","expression":"\"plan_rejected\""},
  {"targetField":"point_count","expression":"plan_pose.point_count"}]`.

Edges: `start→plan_pose` (sequence); `plan_pose→gate` (sequence); `gate→execute`
(`type:"conditional"`, `sourceHandle:"branch-accept"`); `gate→end-rejected`
(`type:"conditional"`, `sourceHandle:"default"`); `execute→read_state` (sequence);
`read_state→end-done` (sequence).

**`tests/plans-gates-executes.json`**:
```json
{ "name":"plans-gates-executes","enabled":true,
  "start_tokens":[{"start_block_id":"start","token":{}}],"human_answers":{},
  "assertions":[
    {"path":"result.value.planned","op":"eq","value":true},
    {"path":"result.value.plan_ok","op":"eq","value":true},
    {"path":"result.value.point_count","op":"gt","value":0},
    {"path":"result.value.planning_time","op":"exists"},
    {"path":"result.value.positions","op":"exists"},
    {"path":"result.value.joint_names","op":"exists"} ] }
```

### Offline gates (S1)
- `just ci::quality-rust` — fmt + clippy (sanity; no Rust changed, but the demo seeder parses
  fixtures at startup — a malformed graph.json surfaces here only if a Rust test loads it).
- `just ci::test-rust` — runs the demo-load + compiler tests; a structurally invalid graph.json or
  an unresolvable guard ref (`plan_pose.success` etc.) fails compilation here **pre-publish** (this
  is the compiler-as-borrow-checker catching a sidecar field-name drift).
- **`just ci::openapi-drift` is NOT required for S1** (no `#[utoipa::path]`/`ToSchema` change) — but
  running it is a cheap confirmation it stays byte-stable.

### LIVE gates (S1)
1. `direnv exec . just dev` then `direnv exec . just dev xarm-up` on slot 5 (rebuild the xArm image
   first — see the Dockerfile edit note).
2. `direnv exec . just dev::openapi` is NOT needed; re-seed the new demo with `mekhan demos reseed`
   (or `just dev reset` if AIR is stale).
3. `mekhan test 00000000-0000-0000-0000-000000000330 -s http://localhost:<slot5_service_port>`.
   **Assert:** all six assertions pass; `result.value.planned == true`, `point_count > 0`,
   `positions` present (readback after execute proves the spliced trajectory actually moved the arm).
4. **The #1 explicit verification (whole-object splice):** confirm the `execute` step received a
   JSON *object* trajectory, not a stringified copy — inspect the runner / `/tmp/motion_bridge.log`
   + the executor job log to confirm `FollowJointTrajectory` was accepted (`GoalStatus==SUCCEEDED`).
   If the goal is rejected as malformed, the whole-object splice failed (Tera rendered the object to
   a non-JSON Display) — fall back to having the bridge return `trajectory` as a JSON string AND
   declaring the execute field as a structured object built field-by-field, OR add a thin compiler
   substitution. (Confirmed-likely-to-work from `backend.rs:496-501` + serde Display, but UNTESTED
   for objects — gate it.)
5. **Gate proof:** temporarily author an unsatisfiable target (force a planning failure → `success`
   false / `point_count` 0) and confirm the run ends at `end-rejected` with `planned:false` and
   `execute` never fired.

### S1 risks & mitigations
- **`ament_python` cannot generate the srv** → use `ament_cmake` (decided above). *Mitigation:* the
  CMakeLists above is the canonical shape; do not "simplify" to ament_python.
- **Sync-over-async deadlock** in the service callback → **MUST** use `MultiThreadedExecutor` +
  `ReentrantCallbackGroup`. *Mitigation:* baked into the node sketch; a single-threaded executor
  hangs forever on the first call.
- **Whole-object splice unproven for objects** → the #1 live gate above; documented fallback.
- **Blob shape mismatch** (RobotTrajectory vs JointTrajectory) → bridge returns the **inner**
  `joint_trajectory` (decided). *Mitigation:* the srv field is named `trajectory` and carries the
  inner message; the execute goal `.trajectory` takes it directly.
- **IK fails for unreachable target** → S1 looks broken on a bad pose. *Mitigation:* pick a target
  near a known SRDF group_state; tune in the first live run; bridge returns finite metrics + a clear
  `error_message` so the gate routes to `end-rejected` cleanly rather than the run wedging.
- **Stale image** → `/plan_to_pose` missing. *Mitigation:* explicit `docker rmi` / rebuild step in
  the live gate.
- **Planning time > 30s default `timeout_ms`** → `call_service` returns `TimedOut`. *Mitigation:* a
  simple pose plan is fast; raise `timeout_ms` in config if any S1 pose is slow (cartesian in S2 may
  need it).
- **Top-level metric NaN** would fail the strict `number` schema gate. *Mitigation:* the bridge
  emits `0` / sentinels on failure, never NaN (baked into the node sketch).

---

## Slice S2 — Collision scene (add_object + plan around an obstacle)

**Goal:** make the planning scene STATEFUL on the runner (decision 1): add a collision object to
move_group's persistent scene, then plan a path that routes around it — proving the scene survives
and influences planning. Also add `plan_cartesian` (straight-line plans).

### NEW files
```
deploy/dev/xarm/motion_bridge/srv/AddObject.srv
deploy/dev/xarm/motion_bridge/srv/RemoveObject.srv
deploy/dev/xarm/motion_bridge/srv/ClearScene.srv
deploy/dev/xarm/motion_bridge/srv/PlanCartesian.srv
demos/34-xarm-scene-plan/demo.json
demos/34-xarm-scene-plan/graph.json
demos/34-xarm-scene-plan/tests/plans-around-obstacle.json
```

`AddObject.srv` request: `string object_id`, `string primitive` (box/sphere/cylinder/mesh),
`float64[] dimensions`, `geometry_msgs/PoseStamped pose`; response: `bool success, string
error_message`. `PlanCartesian.srv` mirrors `PlanToPose` but takes `geometry_msgs/Pose[] waypoints`
and calls `/compute_cartesian_path` (already live, returns a `RobotTrajectory` → inner
`joint_trajectory` blob, same opaque-splice contract).

### EDITS
- `deploy/dev/xarm/motion_bridge/CMakeLists.txt` — add the four new `.srv` files to
  `rosidl_generate_interfaces` (DEPENDENCIES add `moveit_msgs`, `shape_msgs`).
- `deploy/dev/xarm/motion_bridge/scripts/motion_bridge_node.py` — add service handlers:
  `add_object`/`remove_object`/`clear_scene` mutate the scene via `/apply_planning_scene`
  (`ApplyPlanningScene`, `CollisionObject` ADD/REMOVE); `plan_cartesian` via `/compute_cartesian_path`.
- Image rebuild + re-seed as in S1.

### Demo / test
`demos/34-xarm-scene-plan`: `start → add_obstacle (call_service /add_object) → plan_pose (target
behind the obstacle) → gate → execute → read_state → end`. The test asserts `point_count > 0`
(a path was found around the obstacle) and `planned == true`; a control variant placing the obstacle
ON the straight-line path should still succeed (longer trajectory) — assert `total_duration` of the
obstacle run `>` a no-obstacle baseline if you want to *prove* the detour (optional, flaky; the
robust assertion is simply that planning succeeds with the obstacle present).

### Gates
- Offline: `just ci::test-rust` (demo load + compile), `just ci::quality-rust`. No OpenAPI change.
- LIVE: `mekhan test …0340 -s <slot5>`; assert `planned==true`, `point_count>0`. **Scene-persistence
  proof:** run the demo twice without restarting the runner — the second run's `add_object` of the
  same id should be idempotent (or first `clear_scene`) and still plan, confirming the scene is held
  on the live move_group, not re-created per job.

### S2 risks
- **Scene races** if two jobs mutate concurrently → mitigated by `xarm_fleet` instrument/hold
  capacity exclusivity (single writer at the platform layer). S2's single demo is sequential.
- **Obstacle frame mismatch** (`add_object` pose frame ≠ planning frame) → use `link_base`
  consistently; the AddObject `pose` is a `PoseStamped` carrying its own `frame_id`.
- **rosapi typedef crash** on `moveit_msgs`/`shape_msgs` introspection → the existing rosapi seds
  already patch the bounded-sequence + array.array cases; keep AddObject's fields flat
  (`float64[] dimensions` is a plain array, safe).

---

## Slice S3 — Grasp / release + attach (atomic compound bridge ops)

**Goal:** decision 4 — fuse gripper actuation + scene attach/detach into atomic server-side
operations, abstracting the non-standard end-effector.

### NEW files
```
deploy/dev/xarm/motion_bridge/srv/Grasp.srv
deploy/dev/xarm/motion_bridge/srv/Release.srv
demos/35-xarm-grasp-release/demo.json
demos/35-xarm-grasp-release/graph.json
demos/35-xarm-grasp-release/tests/grasps-and-releases.json
```

`Grasp.srv` request: `string object_id`, `float64 width` (or `float64 effort`); response: `bool
success, bool attached, string error_message`. `Release.srv` mirrors it (detach).

### EDITS
- `motion_bridge_node.py` — `grasp` handler: actuate the `xarm_gripper` group (close, via its
  controller / a `FollowJointTrajectory` to the gripper joints) **then** `AttachedCollisionObject`
  ADD on `/apply_planning_scene` — both in one service call, returning only after both complete
  (atomic from the caller's view). `release` = open + detach.
- CMakeLists: add the two `.srv`.
- Image rebuild + re-seed.

**Decision (resolving investigator 1's open question):** grasp/release are **`call_service`
request/response ops**, not actions — they are quick and atomic, and the platform already gets a
clean typed response in `outputs`. If a future real gripper needs feedback during a long close, it
can become a `send_action_goal` (also already supported) without changing the demo's authoring shape
materially. S3 uses `call_service`.

### Demo / test
`demos/35-xarm-grasp-release`: prepend an obstacle/object via `add_object`, plan+execute to its
grasp pose, `grasp` (assert `attached==true`), plan+execute to a place pose, `release` (assert
`attached==false`). Test asserts the two booleans via `result.value.*`.

### Gates
- Offline: `just ci::test-rust`, `just ci::quality-rust`. No OpenAPI change.
- LIVE: `mekhan test …0350 -s <slot5>`; assert grasp `attached==true`, release `attached==false`,
  and a successful execute between them.

### S3 risks
- **Gripper actuation in fake sim** may be a no-op kinematically — assert on the **scene attach
  state** (`AttachedCollisionObject` present in `/get_planning_scene`), which is what matters for
  subsequent collision-aware planning, rather than on physical grip.
- **Atomicity** — the handler must not return success if the attach failed after the gripper moved;
  return a composite `success` AND surface partial state in `error_message`.

---

## Slice S4 — Assets + pick/place/swap SubWorkflows + prepare-cell flow

**Goal:** decisions 5 + 6 — the world model is the asset layer; pick/place/swap are SubWorkflows
over the S1–S3 primitives; the cell is loaded once and persists. MoveIt Task Constructor stays OUT.

### NEW files (asset fixtures — DATA, not code)
```
demos/assets/cell.json
demos/assets/rack.json
demos/assets/instrument_port.json
demos/assets/job.json
demos/assets/inventory.json          # per-run sample inventory (see open Q resolution)
```
Each is an `asset_type` (cardinality `collection`) authored exactly like `demos/assets/metals_db.json`:
- `cell` — fields `object_id`(text), `primitive`(select), `dimensions`(json array-schema),
  `pose`(json pose-schema {frame_id,position[],orientation[]}), `mesh_uri`(file, optional);
  `ref_key:"lab_cell"`.
- `rack` — `slot_id`(text), `grasp_pose`(json pose), `approach_pose`(json pose, optional),
  `occupied`(bool); `ref_key:"sample_rack"`.
- `instrument_port` — `port_id`(text), `instrument`(text), `insert_pose`(json pose),
  `retract_pose`(json pose, optional); `ref_key:"instrument_ports"`.
- `job` — `sample_id`(text), `from_slot`(text), `to_port`(text), `temp_slot`(text, for swap),
  `op`(select pick/place/swap); `ref_key:"run_job"`.
- `inventory` — `sample_id`(text), `slot_id`(text), `dimensions`(json array); `ref_key:"run_inventory"`.

### NEW files (SubWorkflow + flow templates — fixtures)
```
demos/36-prepare-cell/demo.json + graph.json + tests/loads-scene.json
demos/37-pick/demo.json + graph.json            # child SubWorkflow template
demos/38-place/demo.json + graph.json           # child SubWorkflow template
demos/39-swap/demo.json + graph.json            # child SubWorkflow template (composes 37+38)
demos/40-sample-handling/demo.json + graph.json + tests/runs-a-job.json   # top-level
```
templateId numbering continues `…0360`…`…0400` (4-digit block is fine: `…000000000400`).

- **`36-prepare-cell`** — Start → **LeaseScope** (holds xArm instrument/hold capacity) containing
  `clear_scene` (call_service) → a **Map** over the `lab_cell` collection (bound via
  `assetBindings:[{alias:"cell",refKey:"lab_cell"}]`) whose body is an `add_object` call_service step
  reading `{{item.object_id}}/{{item.primitive}}/{{item.dimensions}}/{{item.pose}}` → End
  `{scene_objects:<count>}`. Runs ONCE per session; the scene persists on the live move_group after
  the lease releases (decision 1).
- **`37-pick` / `38-place`** — child templates; Start.initial = their input contract
  (`{grasp_pose, approach_pose, object_id}` / `{insert_pose, approach_pose, object_id}`); body =
  plan_to_pose(approach)→execute→plan_cartesian(grasp/insert)→execute→grasp/release; End
  `{picked:true,object_id}` / `{placed:true}`.
- **`39-swap`** — composes nested SubWorkflows (pick occupant → place to `temp_slot` → pick new
  sample → place to port).
- **`40-sample-handling`** (top-level demo) — binds `sample_rack`/`instrument_ports`/`run_job`/
  `run_inventory`; wraps a **LeaseScope**; a small **`resolve_poses`** python AutomatedStep (binds
  the rack/port collections) does the row lookup (`slot_id == item.from_slot`) **in-code** and emits
  typed pose outputs (collection projection `<ref_key>[*].<field>` is DEFERRED — this is the
  required path); a **Map** over `run_job` with a **Decision** on `{{item.op}}` routes to the
  pick/place/swap **SubWorkflow** nodes, whose `inputMapping` reads `{{resolve_poses.grasp_pose}}`
  etc.

### EDITS to existing files
- **None to Rust.** All asset + SubWorkflow + LeaseScope + Map + Decision machinery is shipped.
- If an asset fixture exercises a field shape the seeder/`CreateAssetTypeRequest` validation rejects,
  the fix is in the *fixture* (data), not code.

### Gates
- Offline: `just ci::test-rust` (demo + asset-fixture load, SubWorkflow compile + child-IO
  derivation, guard resolution), `just ci::quality-rust`.
- **OpenAPI:** S4 adds asset *records/types as fixtures* — **no `#[utoipa::path]`/`ToSchema`
  change**, so `ci::openapi-drift` is **not expected to fail**. (The asset API DTOs already exist
  from docs/20.) Run `just ci::openapi-drift` anyway to confirm byte-stability; if it flags drift,
  something Rust-side changed inadvertently and must be reverted or regenerated.
- LIVE: `mekhan test …0360 -s <slot5>` (prepare-cell loads N objects); then `mekhan test …0400`
  (runs a job end-to-end). Assert pick/place set `picked`/`placed`; assert the scene attach state
  flips; **scene-persistence proof:** run prepare-cell once, then the handling run in a separate
  `mekhan test` invocation — the handling plan must route around the cell objects added by
  prepare-cell, proving cross-run scene persistence on the live move_group.

### S4 risks
- **Collection row lookup** can't be a compile-time ref (`[*]` deferred) → use the `resolve_poses`
  python step (decided). *Mitigation:* this is path (c.1) from the asset investigation — the safe,
  shipped path.
- **Json-object asset field as a constant ref** (`<ref_key>.pose` for an `object`-cardinality asset)
  is proven only for scalars → if you model a singleton (e.g. a `home` pose) as an `object` asset,
  verify the whole-pose-object substitutes as a valid Rhai map literal before relying on it; prefer
  the collection + in-code lookup for anything iterated.
- **SubWorkflow as a Map-body terminal** — nested SubWorkflow inside a Map lane is supported by the
  lowering but should be live-tested for this case; the `swap` nested-SubWorkflow path especially.
- **swap needs a temp slot** → added `temp_slot` to the `job` schema (decided).
- **Scene reset between runs** → prepare-cell adds the static cell once; each handling run
  `add_object`s only its own `run_inventory` and `release`/detaches on completion. Decide per
  deployment whether to `clear_scene` of inventory at run start (recommended: detach run inventory at
  End to keep the static cell clean).

---

## Sequencing & worktree / Workflow execution

- **Branch / slot:** all work lands on `feat/motion-planning`, slot 5. Live testing requires, on
  that slot: `direnv exec . just dev` (full stack up) **and** `direnv exec . just dev xarm-up`
  (xArm sim + xarm6 runner enrolled into `xarm_fleet`). `mekhan test` must target the slot's mekhan
  host port: `mekhan test <templateId> -s http://localhost:<slot5_service_port>` (slot 5 → mekhan
  `20500` per `just/scripts/dev-ports.sh`). **Bash calls in this environment need `direnv exec .`**
  to pick up the slot's ports/env.
- **docs/26 dependency:** RESOLVED — docs/26 is committed on `feat/motion-planning` (`2b094148`),
  so the design doc this plan references lives alongside it on the build branch. (The investigators
  ran in the xarm-pathb checkout, which predated docs/26; that's why earlier drafts flagged a rebase.
  The six decisions used here are the task-prompt restatement of docs/26 — confirmed consistent.)
- **Strict slice ordering:** S1 → S2 → S3 → S4. Each is independently LIVE-GREEN-able and merges on
  its own. S1 is the keystone (proves the bridge + whole-object splice + the gate); do not start S2
  until S1's whole-object-splice live gate passes (S2–S4 all depend on it).
- **Image-rebuild discipline:** every slice that adds a `.srv`/handler changes the xArm image →
  rebuild (`docker rmi aithericon-xarm:jazzy && just dev xarm-up`) before live-testing, or the new
  service silently won't exist.
- **Where a Workflow fans out:** an implementation Workflow should fan out **within a slice**, not
  across slices (slices are sequential). For S1 the natural parallel lanes are: (1) the
  `motion_bridge` ROS package (package.xml/CMakeLists/srv/node) + Dockerfile/entrypoint diffs;
  (2) the `demos/33` graph.json/demo.json/tests authoring. They converge only on the shared field-name
  contract (`trajectory`, `success`, `point_count`, `planning_time`, `total_duration`) — pin those
  names FIRST (they are pinned above) so the two lanes can't drift. S4 fans out widest: the five
  asset fixtures, the three SubWorkflow child templates, and the prepare-cell + top-level flow are
  largely independent lanes converging on the asset `ref_key`s and the child input/output contracts
  (pin `ref_key`s and the `{grasp_pose,approach_pose,object_id}` contracts first). A gated multi-phase
  Workflow (like the prior ROS/capacity capstones) fits: phase per slice, agents per lane, with the
  field-name/contract pinning as the phase-0 spec.

---

## Cross-cutting risks & open questions (consolidated, each with a recommended resolution)

1. **Same image vs separate runner for the bridge?** (inv. 1) → **Same `deploy/dev/xarm` image.**
   The xarm6 runner then advertises both move_group and the bridge under one `xarm_fleet` alias +
   `ros.robot_model==xarm6` cap — no new capacity/Requirements authoring. (Decided.)
2. **Trajectory port: opaque json vs typed RobotTrajectory now?** (inv. 1/2) → **Opaque
   `kind:"json"` (inner JointTrajectory).** Typed port is a deriver-only later upgrade, OUT of S1–S4.
3. **Whole-object Envelope substitution into a string-leaf ref — supported?** (inv. 1/4) → **Yes,
   per `backend.rs:496-501` (`is_pure_placeholder` re-parse of the Tera-rendered object) — but
   UNTESTED for objects.** Make it S1's #1 live gate; documented fallback (bridge returns a JSON
   string + field-by-field structured goal, or a thin compiler substitution) if Tera's object
   Display doesn't round-trip.
4. **Bridge returns full RobotTrajectory or inner JointTrajectory?** (inv. 1/3/4) → **Inner
   `joint_trajectory`** — splices verbatim into `FollowJointTrajectory.goal.trajectory`. (Decided.)
5. **plan_to_pose worst-case planning time vs 30s `timeout_ms`?** (inv. 1) → Simple pose plans are
   fast; **raise `timeout_ms` in config for cartesian/complex variants** (S2). Tune live.
6. **grasp/release: call_service or action?** (inv. 1) → **call_service** (quick, atomic, typed
   response). Switchable to an action later without changing authoring shape. (Decided.)
7. **Real human-task gate in S1?** (inv. 3/4/5) → **No human task; a compiler-checked Decision
   node** on the metrics IS the gate. (Decided — investigator 5's reading over investigator 4's bare
   sequence edge.)
8. **Reject branch: End vs Failure node?** (inv. 5) → **Plain End** (`planned:false`) for
   composability — S4 SubWorkflows branch on `planned`, not catch a NetFailed. (Decided.)
9. **Planning frame `link_base` correct?** (inv. 3) → **Confirm against live `/tf` / move_group's
   `getPlanningFrame` in the first S1 run**; the node defaults to `link_base` but the request can
   override `frame_id`. Tune live.
10. **Service naming convention** (inv. 1/3) → **flat `/plan_to_pose`, `/add_object`, `/grasp`, …**;
    package `aithericon_motion_bridge`. (Decided over the `/motion_bridge/*` namespace.)
11. **Per-run inventory: fold into cell or its own asset?** (inv. 6) → **Its own `inventory` asset**
    (`demos/assets/inventory.json`) distinct from the static `cell` — keeps prepare-cell once-per-
    session and per-run add_object/detach clean. (Decided.)
12. **pick/place/swap: three child templates or one parameterized?** (inv. 6) → **Three separate
    child templates** — crisper derived input/output contracts; the top-level Decision routes by
    `op`. (Decided.)
13. **Sidecar field names** (inv. 2/4/5) → pinned: `success`(bool), `point_count`(number),
    `planning_time`(number, s), `total_duration`(number, s), `trajectory`(json),
    `error_code`(number), `error_message`(text). The gate guard and End mappings key off these
    exactly; a drift fails compilation pre-publish (feature, not bug).
14. **Offline bundled deriver snapshot for the bridge srv?** (inv. 2) → **Defer.** Rely on the live
    `interface_typedefs` path (runner up when authoring); add a bundled snapshot only if offline
    authoring becomes a hard requirement.

---

## OpenAPI / regen obligation

- **S1, S2, S3: NO change** to any `#[utoipa::path]` handler, `ToSchema` DTO, or `IntoParams` query
  type. The entire surface is ROS-package files + on-disk demo fixtures driven through the existing
  `call_service`/`send_action_goal` ROS backend. `just dev::openapi` is **not required**; run
  `just ci::openapi-drift` as a cheap confirmation that `openapi-mekhan.json` /
  `app/src/lib/api/schema.d.ts` stay byte-stable.
- **S4: NO change expected.** Asset types/records are authored as fixtures against the asset API DTOs
  already shipped in docs/20 — adding fixture data does not touch any Rust schema. **`ci::openapi-drift`
  is not expected to fail.** If it does, an unintended Rust change crept in (revert it, or if a DTO
  genuinely changed, run `just dev::openapi` and commit the regenerated `openapi-mekhan.json` +
  `schema.d.ts` together — the contract is enforced in CI).
