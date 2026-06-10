import { describe, it, expect } from 'vitest';
import { resolveEditKind } from './new-capacity-kind';
import type { CapacitySummary } from '$lib/api/capacities';

// Minimal axes stub — only `acceptance` is read by the resolver.
function summary(
	backend: CapacitySummary['backend'],
	acceptance?: 'auto' | 'consent' | null
): Pick<CapacitySummary, 'backend' | 'axes'> {
	const axes =
		acceptance === undefined || acceptance === null
			? acceptance === undefined
				? undefined
				: null
			: ({ acceptance } as unknown as NonNullable<CapacitySummary['axes']>);
	return { backend, axes };
}

describe('resolveEditKind', () => {
	it('presence + consent ⇒ human', () => {
		expect(resolveEditKind(summary('presence', 'consent'))).toBe('human');
	});

	it('presence + auto ⇒ runner_group', () => {
		expect(resolveEditKind(summary('presence', 'auto'))).toBe('runner_group');
	});

	it('presence with null axes ⇒ runner_group', () => {
		expect(resolveEditKind(summary('presence', null))).toBe('runner_group');
	});

	it('presence with absent axes ⇒ runner_group', () => {
		expect(resolveEditKind(summary('presence'))).toBe('runner_group');
	});

	it('tokens ⇒ limit (axes ignored)', () => {
		expect(resolveEditKind(summary('tokens', 'consent'))).toBe('limit');
	});

	it('queue ⇒ worker', () => {
		expect(resolveEditKind(summary('queue', 'auto'))).toBe('worker');
	});

	it('scheduler ⇒ cluster', () => {
		expect(resolveEditKind(summary('scheduler'))).toBe('cluster');
	});

	it('null backend ⇒ worker (name-only fallback)', () => {
		expect(resolveEditKind(summary(null))).toBe('worker');
	});

	it('absent backend ⇒ worker (name-only fallback)', () => {
		expect(resolveEditKind({ axes: undefined })).toBe('worker');
	});
});
