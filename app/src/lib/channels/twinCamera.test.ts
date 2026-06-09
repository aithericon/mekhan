import { describe, it, expect } from 'vitest';
import { Object3D } from 'three';
import { initialCam, rememberCam, frameObject } from './twinCamera';

describe('frameObject', () => {
	it('falls back to a sane default pose for an empty object (no geometry)', () => {
		const cam = frameObject(new Object3D());
		expect(cam.pos).toEqual([0.9, 0.7, 0.9]);
		expect(cam.target).toEqual([0, 0.35, 0]);
	});
});

describe('initialCam / rememberCam — per-key persistence', () => {
	it('frames fresh and does NOT cache when no key is given', () => {
		const a = initialCam(undefined, new Object3D());
		rememberCam(undefined, [1, 2, 3], [4, 5, 6]); // no-op (no key)
		const b = initialCam(undefined, new Object3D());
		expect(b).toEqual(a); // both freshly framed, nothing remembered
	});

	it('caches the framed pose on first build and restores it on rebuild', () => {
		const key = 'edge-1';
		const first = initialCam(key, new Object3D());
		// A later remount with the same key returns the SAME (cached) pose object.
		const second = initialCam(key, new Object3D());
		expect(second).toBe(first);
	});

	it('restores the live orbit pose remembered via OrbitControls change', () => {
		const key = 'edge-2';
		initialCam(key, new Object3D());
		rememberCam(key, [2, 2, 2], [0, 1, 0]);
		const restored = initialCam(key, new Object3D());
		expect(restored.pos).toEqual([2, 2, 2]);
		expect(restored.target).toEqual([0, 1, 0]);
	});

	it('keys are independent — one edge orbit never leaks to another', () => {
		rememberCam('edge-A', [9, 9, 9], [1, 1, 1]);
		const b = initialCam('edge-B', new Object3D()); // fresh frame, not edge-A's pose
		expect(b.pos).not.toEqual([9, 9, 9]);
	});
});
