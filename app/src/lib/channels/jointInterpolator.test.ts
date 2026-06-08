import { describe, it, expect } from 'vitest';
import { alphaFor, stepAngles, createJointInterpolator } from './jointInterpolator';

describe('alphaFor — exponential smoothing factor', () => {
	it('is 0 at dt=0 and approaches 1 as dt grows', () => {
		expect(alphaFor(12, 0)).toBe(0);
		expect(alphaFor(12, 1)).toBeGreaterThan(0.99);
		expect(alphaFor(12, 1)).toBeLessThan(1);
	});

	it('is frame-rate independent — one big step equals many small steps', () => {
		// Easing for 0.1s in one go vs. ten 0.01s ticks must converge to the same
		// remaining distance (this is the whole point of the e^(-rate·dt) form).
		const rate = 12;
		const oneShot = 1 - alphaFor(rate, 0.1); // fraction of distance still left
		let left = 1;
		for (let i = 0; i < 10; i++) left *= 1 - alphaFor(rate, 0.01);
		expect(left).toBeCloseTo(oneShot, 6);
	});
});

describe('stepAngles', () => {
	it('snaps a newly-seen joint to its target (no sweep from origin)', () => {
		const current = new Map<string, number>();
		const target = new Map([['j1', 1.5]]);
		stepAngles(current, target, 0.2);
		expect(current.get('j1')).toBe(1.5);
	});

	it('eases a known joint partway toward its target by alpha', () => {
		const current = new Map([['j1', 0]]);
		const target = new Map([['j1', 1]]);
		stepAngles(current, target, 0.25);
		expect(current.get('j1')).toBeCloseTo(0.25, 6);
		stepAngles(current, target, 0.25);
		expect(current.get('j1')).toBeCloseTo(0.4375, 6); // 0.25 + 0.75*0.25
	});

	it('converges to the target over repeated steps', () => {
		const current = new Map([['j1', 0]]);
		const target = new Map([['j1', 2]]);
		for (let i = 0; i < 200; i++) stepAngles(current, target, 0.2);
		expect(current.get('j1')).toBeCloseTo(2, 6);
	});

	it('leaves a joint absent from the target untouched', () => {
		const current = new Map([['j1', 0.7]]);
		const target = new Map<string, number>(); // j1 dropped this frame
		stepAngles(current, target, 0.5);
		expect(current.get('j1')).toBe(0.7);
	});
});

describe('createJointInterpolator — setTarget validation', () => {
	it('ignores non-finite and length-mismatched entries', () => {
		const interp = createJointInterpolator();
		// Access the target indirectly via a fake robot: setJointValue records calls.
		const calls = new Map<string, number>();
		const robot = {
			setJointValue: (name: string, value: number) => {
				calls.set(name, value);
				return true;
			}
		} as unknown as Parameters<typeof interp.start>[0];

		interp.setTarget(['a', 'b', 'c'], [1, Number.NaN]); // c has no value; b is NaN
		// First step snaps known targets; drive one manual step via a tiny rAF stub.
		const stop = stubRaf(() => interp.start(robot));
		expect(calls.get('a')).toBe(1);
		expect(calls.has('b')).toBe(false); // NaN rejected
		expect(calls.has('c')).toBe(false); // no parallel value
		stop();
	});
});

/** Run one synchronous rAF frame, then restore. Returns the interpolator's stop fn. */
function stubRaf(start: () => () => void): () => void {
	const origRaf = globalThis.requestAnimationFrame;
	const origCancel = globalThis.cancelAnimationFrame;
	let fired = false;
	globalThis.requestAnimationFrame = ((cb: FrameRequestCallback) => {
		if (!fired) {
			fired = true;
			cb(performance.now() + 16);
		}
		return 1;
	}) as typeof requestAnimationFrame;
	globalThis.cancelAnimationFrame = (() => {}) as typeof cancelAnimationFrame;
	const stop = start();
	globalThis.requestAnimationFrame = origRaf;
	globalThis.cancelAnimationFrame = origCancel;
	return stop;
}
