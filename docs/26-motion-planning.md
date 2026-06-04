# 26 — Motion Planning (MoveIt) integration

**Status: DESIGN ONLY** (no code yet). Captures the design dialogue for adding
collision-aware, pose-space motion planning to the platform, oriented toward the
first real use case: **sample placing / repositioning / swapping in a physics
lab** (pick a sample from a rack, place it into an instrument, swap it back).

Builds on the ROS integration arc (`docs/` ROS notes; turtle + xArm Paths A/B)
and reuses the asset layer (`docs/20-resources-and-assets.md`), SubWorkflow
nodes, and the capacity/instrument exclusivity model
(`docs/23-unified-capacity-model.md`).

## The reframe — Path C is not "wire up the MoveGroup action"

The naive framing is "Path C = integrate `moveit_msgs/action/MoveGroup`," which
is where the *goal-too-large-to-hand-author* problem comes from (a
`MotionPlanRequest` is ~200 lines, and turning a target pose into
`goal_constraints` needs MoveIt's own `constructGoalConstraints()` helper).

But we already have the two ends:

- **Path A** (`xarm_planner` services) — plan+execute a **joint** target.
- **Path B** (`FollowJointTrajectory` action) — **execute a trajectory**. Shipped, LIVE-GREEN.

MoveIt's job is to turn a *pose-space, collision-aware goal* into a
**trajectory** — and we already have a trajectory executor. So Path C shrinks to
"add a **plan** primitive that emits a trajectory," and the execute half is free:

```
plan (MoveGroup plan_only)  →  [gate: validate]  →  execute (Path B FollowJointTrajectory)
        ↑ Path C, new                                      ↑ already shipped
```

`plan` mutates nothing on the world; `execute` does. A gate between them is the
whole reason you'd run a lab arm on this platform — "plan → review the trajectory
→ then move," not a fused plan-and-go, next to a cryostat or beamline.

The semantic verbs sample-handling needs — `pick`, `place`, `swap` — are **not**
new backend operations. They are **SubWorkflows** composed over a few primitives:

```
pick(sample, grasp_pose) = open_gripper → plan(approach) → exec → plan_cartesian(grasp) → exec
                           → grasp(sample) → plan(retreat) → exec
```

This gives us MoveIt Task Constructor's value (pick/place pipelines) via
composition, without taking MTC as a dependency. Lab geometry (rack slot poses,
instrument ports) is **data** — i.e. the asset layer.

## The ROS motion stack — where the new piece sits

```
┌─ our scope ─────────────────────────────────────────────────────────┐
│  motion-bridge node      "plan_to_pose(pose) → trajectory + metrics" │  ← NEW (ergonomic facade
│   (a thin facade)         builds the fat MoveIt request, calls         │     over MoveIt; owns the
│                           move_group, holds the live planning scene    │     stateful scene)
└──────────────────────────────────────────────────────────────────────┘
   move_group (MoveIt)      MotionPlanRequest in, collision-free          ← stock
                            trajectory out. The actual planner.
   joint_trajectory_controller   trajectory in, joint setpoints out       ← stock; Path B drives this
   ros2_control + hw driver  joint cmds ↔ the arm (or fake hw).           ← stock — the real
                            THIS is the "MoveIt→robot" translation.           hardware translation
```

Layers below the bridge are all stock ROS / xArm packages, already in the dev
image. The bridge is **not** a hardware translator — it never touches the arm and
never executes motion. It is a *request-shape adapter*: small platform-friendly
call in → MoveIt's verbose request built correctly → planned trajectory out. The
platform calls it with `call_service` (already supported — that was Path A), so
we add no new *backend capability*, only a small ROS node that gives the existing
`call_service` op a nicer thing to call.

## Decisions

### 1. Planning scene state — stateful on the runner
MoveIt's scene is inherently stateful: a grasped sample becomes *attached* to the
gripper link (the planner must then avoid colliding the **sample+arm** with the
world), and the cell is a set of persistent collision objects.

- **Chosen: stateful** — `move_group` stays alive on the runner holding the
  scene; `scene`/`grasp` steps mutate it, `plan` steps plan against current
  state. Single-writer safety comes from the **instrument/hold exclusivity** the
  capacity model already enforces (one workflow owns the arm at a time).
- Rejected: token-pure stateless (every `plan` goal carries the full scene +
  start state). Fights MoveIt's nature, balloons goals, makes `attach` — the
  thing that makes sample-handling work — extremely awkward.

### 2. Trajectory handoff plan→execute — opaque blob + typed metrics
`plan` emits a `moveit_msgs/RobotTrajectory`; its `.joint_trajectory` is exactly
Path B's `FollowJointTrajectory` goal. It rides the net as a data token from
`p_{plan}_data` via a read-arc into the execute step.

- **Chosen: opaque `Json` blob** for the trajectory itself (spliced verbatim into
  the execute goal — mangling it would lose the planner's velocity/accel
  parameterization), **plus a small typed metrics sidecar** (planning time, point
  count, total duration, success/error_code) that gates key on. Dodges running
  the deriver through the deepest nested `moveit_msgs` type, keeps the executed
  trajectory bit-identical to what the planner produced, and aligns with the
  `docs/25` Channel data-plane model.
- **Upgrade path:** fully-typed `RobotTrajectory` port for full inspectability is
  a **deriver-only change** later — no token-flow rework.

### 3. Goal construction — runner-side motion-bridge node
The nasty part of a pose goal is `pose → goal_constraints` (a `PositionConstraint`
bounding volume + an `OrientationConstraint`, relative to the runtime pose) —
can't be templated around a hole.

- **Chosen: a tiny ROS node on the robot image** exposing semantic services
  (`plan_to_pose`, `plan_cartesian`, `grasp`, `release`, `add_object`,
  `remove_object`), calling MoveIt's own helpers with the real URDF/SRDF/IK in
  hand. `executor-ros` stays fully generic (`call_service`/`send_action_goal`
  verbatim). **Zero new backend code.** Also fixes a real gap: stock
  `xarm_pose_plan` stores its plan *inside* move_group and can't return the
  trajectory; the bridge's `plan_to_pose` *returns* the `RobotTrajectory`, which
  is what makes the plan/execute split (decision 2) work.
- Rejected: a Rust `plan` op in `executor-ros` (re-implements
  `constructGoalConstraints`, bakes MoveIt logic into the generic backend) and
  raw MoveGroup JSON (impractical for pose goals). Multi-robot reach without a
  per-robot node is the only real argument for the Rust op; we already ship a
  per-robot image, so the node is a natural home.

### 4. Gripper + attach — atomic `grasp`/`release` on the bridge
Grasping = actuate the gripper (`control_msgs/action/GripperCommand`) **and**
attach in the scene (`/apply_planning_scene`). These must stay consistent: close
without attach → MoveIt thinks the gripper is empty → the *next* plan drives the
sample through the rack.

- **Chosen: atomic compound bridge ops** — `grasp(object_id, [width])` /
  `release(object_id, place_pose)` fuse actuation + scene attach/detach
  server-side. One step, consistency impossible to break. It's the deliberate
  exception to "bridge only plans" (a gripper close has no trajectory to gate),
  and it **abstracts non-standard end-effectors** — cryo-tongs, vacuum pickup,
  tool changer — so `grasp` = "secure the payload" regardless of hardware, and
  the pick/place SubWorkflows don't change when the end-effector does.
- Rejected: orthogonal `send_action_goal` (gripper) + `call_service`
  (apply_planning_scene) steps — pushes ordering/atomicity onto the author.

### 5. World model — everything is an asset; bridge holds only live scene
- **Source of truth = the asset layer** for *all* world geometry & poses:
  cell/environment (bench, instrument bodies, walls — collision primitives/meshes
  with poses), rack layout, instrument ports, and per-run sample inventory.
  Typed, versioned, scoped, reusable; authored by lab users, not roboticists.
- **Bridge = the live MoveIt scene only** (mutable runtime projection). Owns no
  source data; populated *from* assets via `add_object` (the platform reads the
  asset and feeds the bridge — the robot never reaches back into mekhan).
- **Only the robot self-model (URDF/SRDF)** lives on the robot — it's intrinsic
  to the arm, not the environment.
- Ties into decision 1: because the scene is stateful, the **cell is loaded once**
  (a "prepare cell" setup workflow) and **persists**, amortized across every
  subsequent task run; samples are added per run against the already-loaded cell.

```
bridge boot:     (empty scene)
once per session: "prepare cell" workflow → consume cell asset → add_object each
                  element → scene holds the cell, PERSISTS (stateful runner)
per task run:    "swap samples" workflow → stage rack + job assets → add_object
                  each sample at its slot pose → plan / grasp / place / release
```

### 6. MoveIt Task Constructor — out (for now)
MTC's value over SubWorkflow composition is **planning-time backtracking** across
the whole task (try another grasp if the place is unreachable). That only matters
when grasps are **searched** (perception-driven). In the north star, grasp/place
poses are **defined and calibrated in assets** — known-reachable, nothing to
backtrack over.

- **Chosen: compose pick/place/swap as SubWorkflows** over the primitives. Fully
  on-platform (typed contracts, the gate, provenance, causality, visual editing
  per step); no heavy dependency or second task-API surface.
- **Not a one-way door:** if grasping later goes perception-driven, MTC slots
  into the *same* bridge-facade pattern — a `plan_pick(object) → trajectory-seq`
  service that runs MTC internally, called via the existing `call_service`.

## First slice & sequencing

Build smallest-end-to-end-first, de-risking the most novel pieces (the bridge,
the blob handoff, pose-space planning, the gate) before the manipulation stack.

- **Slice 1 — pose-space plan → gate → execute.** Minimal bridge node with one
  service: `plan_to_pose(pose, group) → RobotTrajectory blob + metrics`. Demo:
  `start → plan_to_pose (call_service) → gate (check metrics) → execute (Path B,
  opaque blob) → read_state → end`. Proves: the facade, pose planning, the
  blob handoff, gating on metrics, Path B reuse. No scene/collision, no grasp, no
  assets. Surfaces the `RobotTrajectory` typedef/deriver risk early.
- **Slice 2 — collision scene.** `add_object` + plan around a static obstacle.
  Proves stateful scene + collision-aware planning.
- **Slice 3 — grasp/release + attach.** Atomic manipulation facade; pick up and
  set down an object, planning correctly with it attached.
- **Slice 4 — assets + composition.** Cell asset + "prepare cell" workflow;
  rack/job assets; `pick`/`place`/`swap` as SubWorkflows. The full authoring
  story for sample-handling.

## Deferred / out of scope

- **Real-hardware dogfood** — needs the physical arm powered + networked.
- **Calibration** — aligning asset poses to the real `base_link` frame (touch-off
  or vision). In sim every frame is exact; this is the bridge from
  "sim-correct poses" to "real-world-correct poses." A real-hw task.
- **Fully-typed `RobotTrajectory`** (decision 2 upgrade) — deriver-only, later.
- **MTC / perception-driven grasping** (decision 6) — behind the bridge if/when
  grasps become searched rather than defined.
