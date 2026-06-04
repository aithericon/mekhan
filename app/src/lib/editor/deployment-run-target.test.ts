import { describe, it, expect } from 'vitest';
import {
	resolveRunTarget,
	initialRunTarget,
	capacityTarget,
	targetsByAlias,
	type RunTarget
} from './deployment-run-target';
import type { ResourceSummary } from '$lib/api/resources';

// A minimal `capacity` ResourceSummary stub carrying just the `liveness` axis
// the discriminator reads out of `public_config`.
function cap(path: string, liveness: string): ResourceSummary {
	return {
		id: path,
		path,
		resource_type: 'capacity',
		display_name: path,
		latest_version: 1,
		created_at: '2026-01-01T00:00:00Z',
		updated_at: '2026-01-01T00:00:00Z',
		public_config: { liveness }
	} as unknown as ResourceSummary;
}

// alias → run-target, derived from the loaded capacities' liveness axes.
const targets = targetsByAlias([cap('lab_fleet', 'presence'), cap('gpu_limit', 'seeded')]);

describe('capacityTarget', () => {
	it('maps the liveness axis to a run target', () => {
		expect(capacityTarget(cap('a', 'presence'))).toBe('runner_group');
		expect(capacityTarget(cap('a', 'seeded'))).toBe('limit');
		expect(capacityTarget(cap('a', 'competing_consumer'))).toBe('workers');
	});
	it('falls back to limit for a missing / unknown liveness', () => {
		expect(capacityTarget(cap('a', 'mystery'))).toBe('limit');
		expect(capacityTarget({ ...cap('a', 'x'), public_config: null } as ResourceSummary)).toBe(
			'limit'
		);
	});
});

describe('resolveRunTarget', () => {
	it('maps scheduled → scheduled', () => {
		expect(resolveRunTarget({ mode: 'scheduled', jobTemplate: '' }, targets)).toBe('scheduled');
	});

	it('maps executor with no capacity → workers', () => {
		expect(resolveRunTarget({ mode: 'executor' }, targets)).toBe('workers');
		expect(resolveRunTarget({ mode: 'executor', capacity: null }, targets)).toBe('workers');
	});

	it('resolves a bound alias to its liveness-derived target', () => {
		expect(resolveRunTarget({ mode: 'executor', capacity: { alias: 'lab_fleet' } }, targets)).toBe(
			'runner_group'
		);
		expect(resolveRunTarget({ mode: 'executor', capacity: { alias: 'gpu_limit' } }, targets)).toBe(
			'limit'
		);
	});

	it('REGRESSION: an empty-alias capacity is AMBIGUOUS → null, not workers', () => {
		// This is the bug: selecting "Runner group"/"Concurrency limit" writes
		// capacity:{alias:''} until a resource is chosen. A pure derivation that
		// returned 'workers' here snapped the selector back and hid the picker.
		expect(resolveRunTarget({ mode: 'executor', capacity: { alias: '' } }, targets)).toBeNull();
		expect(resolveRunTarget({ mode: 'executor', capacity: {} }, targets)).toBeNull();
	});

	it('an unresolved alias (resources not loaded / deleted) falls back to limit', () => {
		expect(resolveRunTarget({ mode: 'executor', capacity: { alias: 'ghost' } }, targets)).toBe(
			'limit'
		);
		expect(
			resolveRunTarget(
				{ mode: 'executor', capacity: { alias: 'lab_fleet' } },
				new Map<string, RunTarget>()
			)
		).toBe('limit'); // empty map → unknown alias → provisional limit
	});

	it('undefined value → workers', () => {
		expect(resolveRunTarget(undefined, targets)).toBe('workers');
	});
});

describe('initialRunTarget', () => {
	it('seeds a bound step as a capacity target (no targetsByAlias needed)', () => {
		expect(initialRunTarget({ mode: 'executor', capacity: { alias: 'lab_fleet' } })).toBe('limit');
		expect(initialRunTarget({ mode: 'executor', capacity: { alias: '' } })).toBe('limit');
	});
	it('seeds workers / scheduled correctly', () => {
		expect(initialRunTarget({ mode: 'executor' })).toBe('workers');
		expect(initialRunTarget(undefined)).toBe('workers');
		expect(initialRunTarget({ mode: 'scheduled', jobTemplate: '' })).toBe('scheduled');
	});
});
