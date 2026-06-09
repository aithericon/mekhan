import { describe, it, expect } from 'vitest';
import { resolveEditKind } from './new-capacity-kind';
import type { CapacitySummary } from '$lib/api/capacities';

// Minimal axes stub — only `dispatch` is read by the resolver.
function summary(
	backend: CapacitySummary['backend'],
	dispatch?: 'pull' | 'push' | 'offer' | null
): Pick<CapacitySummary, 'backend' | 'axes'> {
	const axes =
		dispatch === undefined || dispatch === null
			? dispatch === undefined
				? undefined
				: null
			: ({ dispatch } as unknown as NonNullable<CapacitySummary['axes']>);
	return { backend, axes };
}

describe('resolveEditKind', () => {
	it('presence + offer ⇒ human', () => {
		expect(resolveEditKind(summary('presence', 'offer'))).toBe('human');
	});

	it('presence + pull ⇒ runner_group', () => {
		expect(resolveEditKind(summary('presence', 'pull'))).toBe('runner_group');
	});

	it('presence + push ⇒ runner_group', () => {
		expect(resolveEditKind(summary('presence', 'push'))).toBe('runner_group');
	});

	it('presence with null axes ⇒ runner_group', () => {
		expect(resolveEditKind(summary('presence', null))).toBe('runner_group');
	});

	it('presence with absent axes ⇒ runner_group', () => {
		expect(resolveEditKind(summary('presence'))).toBe('runner_group');
	});

	it('tokens ⇒ limit (axes ignored)', () => {
		expect(resolveEditKind(summary('tokens', 'offer'))).toBe('limit');
	});

	it('queue ⇒ worker', () => {
		expect(resolveEditKind(summary('queue', 'pull'))).toBe('worker');
	});

	it('scheduler ⇒ cluster', () => {
		expect(resolveEditKind(summary('scheduler'))).toBe('cluster');
	});

	it('deferred ⇒ worker', () => {
		expect(resolveEditKind(summary('deferred'))).toBe('worker');
	});
});
