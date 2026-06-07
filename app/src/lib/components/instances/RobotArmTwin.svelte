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
	import { Box3, LoadingManager, Mesh, MeshStandardMaterial, Object3D, Vector3 } from 'three';
	import { STLLoader } from 'three/examples/jsm/loaders/STLLoader.js';
	import URDFLoader, { type URDFRobot } from 'urdf-loader';
	import { loadRobotDescription } from '$lib/channels/robotDescription';
	import type { UrdfJointFrame } from '$lib/channels/urdfStreamPlayer';

	let {
		robotModel,
		frame,
		frozen = false
	}: { robotModel: string | null; frame: UrdfJointFrame | null; frozen?: boolean } = $props();

	let robot = $state<URDFRobot | null>(null);
	let error = $state<string | null>(null);

	// Camera framing — derived from the loaded robot's bounding box so the arm
	// fills the stage at whatever size/aspect the edge widget gives us, instead of
	// a fixed pose that leaves dead space when the container grows.
	const FOV = 35;
	let camPos = $state<[number, number, number]>([0.9, 0.7, 0.9]);
	let camTarget = $state<[number, number, number]>([0, 0.35, 0]);

	// Fit the camera to the robot's AABB: centre on it, back off just far enough
	// (plus a small margin) that the tallest reach fits in the FOV. Framed once at
	// load using the home pose (the arm's largest extent), so subsequent motion
	// always stays in view without the camera jumping.
	function frameRobot(r: URDFRobot) {
		r.updateMatrixWorld(true);
		const box = new Box3().setFromObject(r);
		if (box.isEmpty()) return;
		const center = box.getCenter(new Vector3());
		const size = box.getSize(new Vector3());
		const maxDim = Math.max(size.x, size.y, size.z) || 1;
		const dist = (maxDim / 2 / Math.tan(((FOV * Math.PI) / 180) / 2)) * 1.2;
		const dir = new Vector3(1, 0.5, 1).normalize();
		camTarget = [center.x, center.y, center.z];
		camPos = [center.x + dir.x * dist, center.y + dir.y * dist, center.z + dir.z * dist];
	}

	// Resolve a `package://…` (or already-resolved) mesh path back to the bytes in
	// the unzipped bundle. Keys are full archive paths, e.g.
	// `xarm_description/meshes/xarm6/visual/link1.stl`. We try, in order: exact
	// match, the substring from the package root, then a basename suffix match —
	// robust to whatever prefix urdf-loader prepends.
	function lookupMesh(meshes: Map<string, Uint8Array>, path: string): Uint8Array | null {
		if (meshes.has(path)) return meshes.get(path) ?? null;
		const pkgIdx = path.indexOf('xarm_description/');
		if (pkgIdx >= 0) {
			const k = path.slice(pkgIdx);
			if (meshes.has(k)) return meshes.get(k) ?? null;
		}
		const base = path.split(/[\\/]/).pop() ?? '';
		if (base) {
			for (const [k, v] of meshes) {
				if (k === base || k.endsWith('/' + base)) return v;
			}
		}
		return null;
	}

	function buildRobot(urdfText: string, meshes: Map<string, Uint8Array>): URDFRobot {
		const manager = new LoadingManager();
		const loader = new URDFLoader(manager);
		// Map the package name to itself so urdf-loader's `package://xarm_description/…`
		// resolves to a path our bundle keys match on.
		loader.packages = { xarm_description: 'xarm_description' };
		loader.loadMeshCb = (path: string, _mgr: LoadingManager, onLoad: (mesh: Object3D, err?: Error) => void) => {
			try {
				const bytes = lookupMesh(meshes, path);
				if (!bytes) {
					// Keep the link in the tree (empty placeholder) but flag the miss.
					onLoad(new Object3D(), new Error(`mesh not in bundle: ${path}`));
					return;
				}
				// STLLoader wants an ArrayBuffer; slice to the exact view so a shared
				// underlying buffer (from fflate) doesn't leak adjacent entries.
				const ab = bytes.buffer.slice(
					bytes.byteOffset,
					bytes.byteOffset + bytes.byteLength
				) as ArrayBuffer;
				const geometry = new STLLoader().parse(ab);
				const mesh = new Mesh(
					geometry,
					new MeshStandardMaterial({ color: 0xc8ccd0, metalness: 0.25, roughness: 0.55 })
				);
				onLoad(mesh);
			} catch (e) {
				onLoad(new Object3D(), e as Error);
			}
		};
		const r = loader.parse(urdfText);
		r.rotation.x = -Math.PI / 2; // ROS Z-up → three Y-up
		return r;
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
				frameRobot(r);
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

	// Apply each incoming joint frame to the model. Drop-to-latest: `frame` is the
	// newest pose the player parsed; we set every named arm joint we recognise.
	$effect(() => {
		const r = robot;
		const f = frame;
		if (!r || !f) return;
		const n = Math.min(f.joint_names.length, f.positions.length);
		for (let i = 0; i < n; i++) {
			const angle = f.positions[i];
			if (typeof angle === 'number' && Number.isFinite(angle)) {
				r.setJointValue(f.joint_names[i], angle);
			}
		}
	});
</script>

<div class="twin" class:frozen>
	<Canvas renderMode="always">
		<T.PerspectiveCamera makeDefault position={camPos} fov={FOV}>
			<OrbitControls enableDamping enablePan={false} target={camTarget} />
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
		background: radial-gradient(circle at 50% 35%, #1b2027 0%, #0e1116 100%);
		border-radius: 4px;
		overflow: hidden;
	}
	.twin.frozen {
		filter: saturate(0.6) brightness(0.85);
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
