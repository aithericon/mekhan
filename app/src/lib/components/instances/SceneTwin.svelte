<!--
  SceneTwin — a live 3D twin of a robot PLANNING SCENE: the URDF arm plus the
  collision-object primitives MoveIt plans against (the work-cell samples), plus
  any object attached to (riding) the gripper. The planning-scene sibling of
  `RobotArmTwin` (which renders only the arm from a joint-state stream): where
  that decodes `{joint_names, positions}`, this decodes a full planning-scene
  snapshot `{ joints, objects, attached }` (one NDJSON object per tick) into the
  arm pose AND the live collision geometry around it.

  Pipeline: `robotModel` → `loadRobotDescription()` (URDF text + unzipped mesh
  bytes, asset-resolved + memoised) → `buildRobot()` (shared with RobotArmTwin) →
  a THREE `URDFRobot`. The robot and a world-objects `<T.Group>` hang under ONE
  common parent that carries the ROS Z-up → three Y-up rotation, so the world
  collision objects (authored in the `link_base` ROS frame) share the arm's
  coordinate space exactly. Each `frame` change applies the joint angles and
  rebuilds the object meshes (object counts are tiny, so a full rebuild per frame
  is the simplest correct refresh). Attached objects are parented to their URDF
  link Object3D so a grasped sample rides the gripper automatically.

  Materials read the geometry at a glance: gray = the arm (urdf-loader's mesh
  material), warm amber = world collision objects ("the work"), green = a grasped
  (attached) sample.
-->
<script lang="ts">
	import { Canvas, T } from '@threlte/core';
	import { OrbitControls } from '@threlte/extras';
	import {
		BoxGeometry,
		CylinderGeometry,
		Group,
		Mesh,
		MeshStandardMaterial,
		Quaternion,
		SphereGeometry,
		type Object3D
	} from 'three';
	import { type URDFRobot } from 'urdf-loader';
	import { loadRobotDescription } from '$lib/channels/robotDescription';
	import { FOV, buildRobot, frameRobot } from '$lib/channels/robotModel';
	import { createJointInterpolator } from '$lib/channels/jointInterpolator';
	import type { ScenePrimitive, ScenePose, SceneFrame } from '$lib/channels/sceneStreamPlayer';

	/** A group placed at a ROS pose (origin), to nest primitive meshes under so the
	 *  object/primitive transforms compose (MoveIt parks the world transform on the
	 *  object origin and zeroes the primitive poses; composing both is always right). */
	function poseGroup(pose: ScenePose): Group {
		const g = new Group();
		g.position.set(pose.position.x, pose.position.y, pose.position.z);
		g.quaternion.set(pose.orientation.x, pose.orientation.y, pose.orientation.z, pose.orientation.w);
		return g;
	}

	let {
		robotModel,
		frame,
		frozen = false
	}: { robotModel: string | null; frame: SceneFrame | null; frozen?: boolean } = $props();

	let robot = $state<URDFRobot | null>(null);
	let error = $state<string | null>(null);

	// Common parent of the robot AND the world collision objects — carries the
	// ROS Z-up → three Y-up rotation so both live in the SAME coordinate space.
	// (buildRobot also self-rotates the robot; we zero the robot's own rotation
	// once it's reparented here so it isn't double-rotated — see the load effect.)
	const sceneRoot = new Group();
	sceneRoot.rotation.x = -Math.PI / 2;
	// World collision objects hang here (rebuilt each frame). Distinct group so we
	// can clear it without touching the robot.
	const worldGroup = new Group();
	sceneRoot.add(worldGroup);
	// Attached objects are parented to a moving URDF link (not worldGroup), so they
	// can't be cleared via clearGroup(worldGroup). Track their origin groups and
	// detach + dispose the previous set each frame — otherwise one green mesh
	// accumulates per tick on the gripper link (leak + z-fighting).
	let attachedGroups: Group[] = [];

	// SolidPrimitive type ints (shape_msgs/SolidPrimitive).
	const BOX = 1;
	const SPHERE = 2;
	const CYLINDER = 3;

	// Camera framing — shared `frameRobot` against the loaded robot's AABB.
	let camPos = $state<[number, number, number]>([0.9, 0.7, 0.9]);
	let camTarget = $state<[number, number, number]>([0, 0.35, 0]);

	// World collision objects = warm amber, slightly translucent ("the work").
	const WORLD_COLOR = 0xf59e0b;
	// Attached (grasped) objects = green, so a sample riding the gripper stands out.
	const ATTACHED_COLOR = 0x22c55e;

	/** Build a THREE mesh for one collision primitive + pose, or null if unknown. */
	function buildPrimitive(prim: ScenePrimitive, pose: ScenePose, color: number): Mesh | null {
		const d = prim.dimensions;
		let geometry;
		if (prim.type === BOX) {
			geometry = new BoxGeometry(d[0] ?? 0, d[1] ?? 0, d[2] ?? 0);
		} else if (prim.type === SPHERE) {
			geometry = new SphereGeometry(d[0] ?? 0, 24, 16);
		} else if (prim.type === CYLINDER) {
			// URDF/ROS cylinder dims are [height, radius]; three CylinderGeometry is
			// (radiusTop, radiusBottom, height). ROS cylinders are Z-aligned, three
			// cylinders are Y-aligned — rotate +90° about X to match.
			geometry = new CylinderGeometry(d[1] ?? 0, d[1] ?? 0, d[0] ?? 0, 24);
		} else {
			return null;
		}
		const mesh = new Mesh(
			geometry,
			new MeshStandardMaterial({ color, metalness: 0.1, roughness: 0.6, transparent: true, opacity: 0.85 })
		);
		mesh.position.set(pose.position.x, pose.position.y, pose.position.z);
		mesh.quaternion.copy(
			new Quaternion(pose.orientation.x, pose.orientation.y, pose.orientation.z, pose.orientation.w)
		);
		if (prim.type === CYLINDER) mesh.rotateX(Math.PI / 2);
		return mesh;
	}

	/** Dispose every mesh under a group and detach it (avoid GPU leak on rebuild). */
	function clearGroup(group: Object3D) {
		for (const child of [...group.children]) {
			group.remove(child);
			const mesh = child as Mesh;
			mesh.geometry?.dispose?.();
			const mat = mesh.material;
			if (Array.isArray(mat)) mat.forEach((m) => m.dispose?.());
			else mat?.dispose?.();
		}
	}

	/** Remove every mounted arm from sceneRoot (keep the persistent worldGroup),
	 *  disposing its meshes. Without this, re-running the load effect or losing the
	 *  model leaves prior arms in the scene frozen at past poses — the ghost fan. */
	function clearArms() {
		for (const child of [...sceneRoot.children]) {
			if (child === worldGroup) continue;
			sceneRoot.remove(child);
			child.traverse((o) => {
				const m = o as Mesh;
				m.geometry?.dispose?.();
				const mat = m.material;
				if (Array.isArray(mat)) mat.forEach((x) => x.dispose?.());
				else mat?.dispose?.();
			});
		}
	}

	// (Re)load the URDF whenever the robot model changes. Once built we reparent it
	// under `sceneRoot` (which carries the Z-up→Y-up rotation) and zero the robot's
	// own rotation so it isn't rotated twice.
	$effect(() => {
		const model = robotModel;
		if (!model) {
			clearArms();
			robot = null;
			error = null;
			return;
		}
		let cancelled = false;
		void (async () => {
			try {
				const desc = await loadRobotDescription(model);
				if (cancelled) return;
				if (!desc) {
					error = `no robot description for "${model}"`;
					robot = null;
					return;
				}
				const r = buildRobot(desc.urdfText, desc.meshes);
				// frameRobot expects the robot's own Z-up→Y-up rotation in place.
				({ camPos, camTarget } = frameRobot(r));
				// The common parent now carries that rotation; clear the robot's own so
				// the world objects (also under sceneRoot) share its exact frame.
				r.rotation.set(0, 0, 0);
				// Remove any arm already mounted before adding the new one — otherwise a
				// re-run of this effect (robotModel re-eval / null↔model flicker) STACKS
				// arms: the old one stays in sceneRoot frozen at its last joint pose while
				// the new one renders live, fanning out as ghost arms.
				clearArms();
				sceneRoot.add(r);
				robot = r;
				error = null;
			} catch (e) {
				if (!cancelled) {
					error = e instanceof Error ? e.message : String(e);
					robot = null;
				}
			}
		})();
		return () => {
			cancelled = true;
		};
	});

	// Arm pose is SMOOTHED (see RobotArmTwin): the planning-scene stream arrives at a
	// few Hz, so easing the joints toward the latest target each animation frame
	// avoids the teleport-between-poses stutter. The attached (grasped) sample is
	// parented to a URDF link, so it rides the gripper smoothly once the joints glide.
	const interp = createJointInterpolator();

	// Feed the latest joint target from each scene snapshot (drop-to-latest).
	$effect(() => {
		if (frame) interp.setTarget(frame.joints.names, frame.joints.positions);
	});

	// Run the rAF easing loop while a robot is mounted.
	$effect(() => {
		const r = robot;
		if (!r) return;
		return interp.start(r);
	});

	// Rebuild the world + attached collision geometry on each incoming snapshot.
	// Drop-to-latest: `frame` is the newest snapshot the player parsed. Joint angles
	// are handled by the interpolator above; this effect owns only the object meshes.
	$effect(() => {
		const r = robot;
		const f = frame;
		if (!r || !f) return;

		// 2. World collision objects — full rebuild (object counts are tiny). Clear
		//    the world group and re-add, per object, a pose group at its origin with
		//    a mesh per primitive nested under it (origin ∘ primitive_pose).
		clearGroup(worldGroup);
		for (const obj of f.objects) {
			const g = poseGroup(obj.origin);
			const count = Math.min(obj.primitives.length, obj.poses.length);
			for (let i = 0; i < count; i++) {
				const mesh = buildPrimitive(obj.primitives[i], obj.poses[i], WORLD_COLOR);
				if (mesh) g.add(mesh);
			}
			worldGroup.add(g);
		}

		// 3. Attached objects — parent each under its URDF link so a grasped sample
		//    rides the gripper once the joints are applied. The object's origin is
		//    relative to the link frame; primitive poses compose under it. Fall back
		//    to the world group if the named link can't be resolved (don't crash).
		//    Detach + dispose the prior set first (these hang off moving links, not
		//    worldGroup, so clearGroup misses them — without this they accumulate).
		for (const g of attachedGroups) {
			g.removeFromParent();
			clearGroup(g);
		}
		attachedGroups = [];
		for (const att of f.attached) {
			const link: Object3D | undefined = r.links?.[att.link];
			const parent = link ?? worldGroup;
			const g = poseGroup(att.origin);
			const count = Math.min(att.primitives.length, att.poses.length);
			for (let i = 0; i < count; i++) {
				const mesh = buildPrimitive(att.primitives[i], att.poses[i], ATTACHED_COLOR);
				if (mesh) g.add(mesh);
			}
			parent.add(g);
			attachedGroups.push(g);
		}
	});
</script>

<!-- Capture pointer/wheel so OrbitControls drives the camera without the events
     bubbling to the xyflow pane (which would pan/zoom the whole graph). The
     canvas (OrbitControls' listener target) still receives them in the target
     phase before we stop propagation on the way up. -->
<div
	class="twin"
	class:frozen
	role="presentation"
	onpointerdown={(e) => e.stopPropagation()}
	onwheel={(e) => e.stopPropagation()}
	ondblclick={(e) => e.stopPropagation()}
	oncontextmenu={(e) => e.stopPropagation()}
>
	<Canvas renderMode="always">
		<T.PerspectiveCamera makeDefault position={camPos} fov={FOV}>
			<OrbitControls enableDamping enablePan={false} target={camTarget} />
		</T.PerspectiveCamera>
		<T.AmbientLight intensity={0.75} />
		<T.DirectionalLight position={[3, 5, 2]} intensity={1.5} />
		<T.DirectionalLight position={[-2, 2, -3]} intensity={0.5} />
		<T.GridHelper args={[2, 12, 0x3a4049, 0x262b31]} />
		<T is={sceneRoot} />
	</Canvas>
	{#if error}
		<div class="err" title={error}>3D: {error}</div>
	{/if}
</div>

<style>
	.twin {
		position: relative;
		width: 100%;
		height: 100%;
		min-width: 160px;
		min-height: 120px;
		/* Brighter than the dark xyflow canvas so the stage reads as a distinct
		   frame instead of blending into the page background. */
		background: radial-gradient(circle at 50% 32%, #36404d 0%, #1c232c 100%);
		border-radius: 4px;
		overflow: hidden;
	}
	/* Threlte sizes the canvas from getBoundingClientRect (the zoom-SCALED size) and
	   doesn't re-measure on xyflow zoom — ResizeObserver ignores CSS transforms — so
	   the canvas otherwise keeps its mount-time pixel size and leaves a gap when the
	   edge preview scales. Pin it to fill .twin so it always tracks the container. */
	.twin :global(canvas) {
		display: block;
		width: 100% !important;
		height: 100% !important;
	}
	.twin.frozen {
		/* Frozen (passive) keeps the SAME slate stage colour as the live state —
		   the ended state is signalled by the "ended" badge + frame border, not by
		   muting the background. Only a hair dimmer. */
		filter: brightness(0.96);
	}
	.err {
		position: absolute;
		left: 0;
		right: 0;
		bottom: 0;
		padding: 2px 4px;
		font-size: 9px;
		line-height: 1.2;
		color: #fca5a5;
		background: rgba(0, 0, 0, 0.55);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
</style>
