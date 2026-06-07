/**
 * Shared URDF model-building helpers — the framework-agnostic core of the 3D
 * robot twins. Both {@link import('$lib/components/instances/RobotArmTwin.svelte')}
 * (the joint-state twin) and the planning-scene `SceneTwin` build their
 * `URDFRobot` through this module so the URDF parse, mesh resolution, and camera
 * framing live in exactly ONE place.
 *
 * Pipeline: a `robot_description` bundle (URDF text + unzipped mesh bytes,
 * asset-resolved via {@link import('./robotDescription').loadRobotDescription}) →
 * urdf-loader parses the URDF, resolving each `package://…` mesh from the
 * in-memory bundle via an STLLoader `loadMeshCb` → a THREE `URDFRobot` Object3D.
 *
 * URDF/ROS is Z-up; three.js is Y-up — `buildRobot` rotates the robot -90° about
 * X so it stands upright. Mesh parsing is synchronous (bytes are already in
 * memory), so the robot is fully built in one `loader.parse` with no async mesh
 * races.
 */
import { Box3, LoadingManager, Mesh, MeshStandardMaterial, Object3D, Vector3 } from 'three';
import { STLLoader } from 'three/examples/jsm/loaders/STLLoader.js';
import URDFLoader, { type URDFRobot } from 'urdf-loader';

/** Camera FOV the twins frame against — shared so RobotArmTwin & SceneTwin match. */
export const FOV = 35;

/**
 * Resolve a `package://…` (or already-resolved) mesh path back to the bytes in
 * the unzipped bundle. Keys are full archive paths, e.g.
 * `xarm_description/meshes/xarm6/visual/link1.stl`. We try, in order: exact
 * match, the substring from the package root, then a basename suffix match —
 * robust to whatever prefix urdf-loader prepends.
 */
export function lookupMesh(meshes: Map<string, Uint8Array>, path: string): Uint8Array | null {
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

export function buildRobot(urdfText: string, meshes: Map<string, Uint8Array>): URDFRobot {
	const manager = new LoadingManager();
	const loader = new URDFLoader(manager);
	// Map the package name to itself so urdf-loader's `package://xarm_description/…`
	// resolves to a path our bundle keys match on.
	loader.packages = { xarm_description: 'xarm_description' };
	loader.loadMeshCb = (
		path: string,
		_mgr: LoadingManager,
		onLoad: (mesh: Object3D, err?: Error) => void
	) => {
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

/**
 * Fit the camera to the robot's AABB: centre on it, back off just far enough
 * (plus a small margin) that the tallest reach fits in the FOV. Framed once at
 * load using the home pose (the arm's largest extent), so subsequent motion
 * always stays in view without the camera jumping.
 */
export function frameRobot(r: URDFRobot): {
	camPos: [number, number, number];
	camTarget: [number, number, number];
} {
	r.updateMatrixWorld(true);
	const box = new Box3().setFromObject(r);
	if (box.isEmpty()) return { camPos: [0.9, 0.7, 0.9], camTarget: [0, 0.35, 0] };
	const center = box.getCenter(new Vector3());
	const size = box.getSize(new Vector3());
	const maxDim = Math.max(size.x, size.y, size.z) || 1;
	const dist = (maxDim / 2 / Math.tan(((FOV * Math.PI) / 180) / 2)) * 1.2;
	const dir = new Vector3(1, 0.5, 1).normalize();
	return {
		camTarget: [center.x, center.y, center.z],
		camPos: [center.x + dir.x * dist, center.y + dir.y * dist, center.z + dir.z * dist]
	};
}
