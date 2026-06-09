/**
 * Per-edge camera persistence for the 3D twins. The twin widgets are torn down
 * and rebuilt as the graph is panned/zoomed (the LOD + viewport gate in
 * EdgeMediaWidget unmounts off-screen / zoomed-out feeds). A fresh twin re-frames
 * the camera to its computed default, so without this the user's orbit/zoom would
 * snap back to the default every time the widget remounts — the "view jumps /
 * feels unstable while panning" symptom.
 *
 * This module-level cache keys the last camera pose by a stable view key (the
 * edge id). On (re)build the twin restores the cached pose instead of re-framing;
 * OrbitControls' `change` event writes the live pose back. The cache is tiny
 * (one entry per twin edge) and intentionally process-lived — it survives a
 * widget remount but not a page reload, which is exactly the desired scope.
 */
import { Box3, Vector3, type Object3D } from 'three';
import { FOV } from './robotModel';

export interface CamState {
	pos: [number, number, number];
	target: [number, number, number];
}

const cache = new Map<string, CamState>();

/**
 * Fit the camera to a robot's AABB: centre on it and back off (with a margin) so
 * the tallest reach fits the FOV. Generalised from the twins' old `frameRobot`
 * (which assumed a `URDFRobot`); any `Object3D` works, so the scene twin can frame
 * against the whole scene root if it ever wants to.
 */
export function frameObject(o: Object3D): CamState {
	o.updateMatrixWorld(true);
	const box = new Box3().setFromObject(o);
	if (box.isEmpty()) return { pos: [0.9, 0.7, 0.9], target: [0, 0.35, 0] };
	const center = box.getCenter(new Vector3());
	const size = box.getSize(new Vector3());
	const maxDim = Math.max(size.x, size.y, size.z) || 1;
	const dist = (maxDim / 2 / Math.tan(((FOV * Math.PI) / 180) / 2)) * 1.2;
	const dir = new Vector3(1, 0.5, 1).normalize();
	return {
		target: [center.x, center.y, center.z],
		pos: [center.x + dir.x * dist, center.y + dir.y * dist, center.z + dir.z * dist]
	};
}

/**
 * Initial camera for a freshly built twin: the cached pose for `key` if we have
 * one (preserves the user's orbit across remounts), otherwise frame `o` and seed
 * the cache. A null/empty key (no stable id) always frames fresh — no caching.
 */
export function initialCam(key: string | undefined, o: Object3D): CamState {
	if (key) {
		const cached = cache.get(key);
		if (cached) return cached;
	}
	const framed = frameObject(o);
	if (key) cache.set(key, framed);
	return framed;
}

/** Remember the live camera pose for `key` (called from OrbitControls `change`). */
export function rememberCam(
	key: string | undefined,
	pos: [number, number, number],
	target: [number, number, number]
): void {
	if (key) cache.set(key, { pos, target });
}
