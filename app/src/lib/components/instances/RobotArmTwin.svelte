<!--
  RobotArmTwin — a live 3D URDF "twin" of a robot arm, driven by a joint-angle
  stream. The presentation-side analog of the `<video>`/`<img>` media elements in
  EdgeMediaWidget: where those decode bytes, this decodes a structured joint-state
  stream (one `{joint_names, positions}` frame at a time) into joint rotations on a
  real URDF model loaded from the `robot_description` asset.

  Pipeline: `robotModel` → `loadRobotDescription()` (URDF text + unzipped mesh
  bytes, asset-resolved + memoised) → urdf-loader parses the URDF, resolving each
  `package://…` mesh from the in-memory bundle via an STLLoader `loadMeshCb` → a
  THREE `URDFRobot` Object3D rendered in a Threlte `<Canvas>`. Each `frame` prop
  change applies `setJointValue(name, angle)` for the arm joints.

  URDF/ROS is Z-up; three.js is Y-up — the robot is rotated -90° about X so it
  stands upright. Mesh parsing is synchronous (bytes are already in memory), so the
  robot is fully built in one `loader.parse` with no async mesh races.
-->
<script lang="ts">
	import { Canvas, T } from '@threlte/core';
	import { OrbitControls } from '@threlte/extras';
	import { NoToneMapping } from 'three';
	import type { OrbitControls as ThreeOrbitControls } from 'three/examples/jsm/controls/OrbitControls.js';
	import { type URDFRobot } from 'urdf-loader';
	import { loadRobotDescription } from '$lib/channels/robotDescription';
	import { FOV, buildRobot } from '$lib/channels/robotModel';
	import { initialCam, rememberCam } from '$lib/channels/twinCamera';
	import { createJointInterpolator } from '$lib/channels/jointInterpolator';
	import type { UrdfJointFrame } from '$lib/channels/urdfStreamPlayer';

	let {
		robotModel,
		frame,
		frozen = false,
		// Stable id for camera persistence across remounts (the edge id). Without it
		// each rebuild re-frames the camera, snapping the user's orbit back.
		viewKey,
		// Render pixel ratio — driven by the host's graph zoom so the WebGL buffer
		// tracks the on-screen (CSS-transform-scaled) size instead of staying frozen
		// at mount resolution (which blurs when the graph is zoomed in).
		dpr
	}: {
		robotModel: string | null;
		frame: UrdfJointFrame | null;
		frozen?: boolean;
		viewKey?: string;
		dpr?: number;
	} = $props();

	let robot = $state<URDFRobot | null>(null);
	let error = $state<string | null>(null);

	// Camera framing — the cached pose for this edge if we have one (preserves the
	// user's orbit across remounts), else framed to the robot's AABB so the arm
	// fills the stage at whatever size the edge widget gives us.
	let camPos = $state<[number, number, number]>([0.9, 0.7, 0.9]);
	let camTarget = $state<[number, number, number]>([0, 0.35, 0]);
	let controls = $state<ThreeOrbitControls | undefined>();

	// Snapshot the live camera pose so a later remount restores it (not the default).
	function onControlsChange() {
		const c = controls;
		if (!c) return;
		rememberCam(
			viewKey,
			c.object.position.toArray() as [number, number, number],
			c.target.toArray() as [number, number, number]
		);
	}

	// (Re)load the URDF whenever the robot model changes. Synchronous mesh parsing
	// means once `loadRobotDescription` resolves, the robot is ready in one tick.
	$effect(() => {
		const model = robotModel;
		if (!model) {
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
				({ pos: camPos, target: camTarget } = initialCam(viewKey, r));
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

	// Smooth the joint motion: streams arrive at the ROS emit rate (a few Hz), so
	// applying each frame directly teleports the arm between poses (visible stutter).
	// The interpolator eases every joint toward the latest target each animation
	// frame, decoupling visual smoothness from data rate.
	const interp = createJointInterpolator();

	// Feed the latest target pose. Drop-to-latest: `frame` is the newest the player
	// parsed; the easing loop carries the motion between frames.
	$effect(() => {
		if (frame) interp.setTarget(frame.joint_names, frame.positions);
	});

	// Run the rAF easing loop while a robot is mounted. `renderMode="always"` redraws
	// every frame, so the eased joint mutations show without an explicit invalidate.
	$effect(() => {
		const r = robot;
		if (!r) return;
		return interp.start(r);
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
	<Canvas renderMode="always" toneMapping={NoToneMapping} {dpr}>
		<T.PerspectiveCamera makeDefault position={camPos} fov={FOV}>
			<OrbitControls
				bind:ref={controls}
				onchange={onControlsChange}
				enableDamping
				enablePan={false}
				target={camTarget}
			/>
		</T.PerspectiveCamera>
		<T.AmbientLight intensity={0.75} />
		<T.DirectionalLight position={[3, 5, 2]} intensity={1.5} />
		<T.DirectionalLight position={[-2, 2, -3]} intensity={0.5} />
		<T.GridHelper args={[2, 12, 0x3a4049, 0x262b31]} />
		{#if robot}
			<T is={robot} />
		{/if}
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
