import { describe, it, expect } from 'vitest';
import { resolveRunTarget, initialRunTarget } from './deployment-run-target';

const kinds = new Map<string, 'runner_group' | 'concurrency_limit'>([
	['lab_fleet', 'runner_group'],
	['gpu_limit', 'concurrency_limit']
]);

describe('resolveRunTarget', () => {
	it('maps scheduled → scheduled', () => {
		expect(resolveRunTarget({ mode: 'scheduled', jobTemplate: '' }, kinds)).toBe('scheduled');
	});

	it('maps executor with no capacity → workers', () => {
		expect(resolveRunTarget({ mode: 'executor' }, kinds)).toBe('workers');
		expect(resolveRunTarget({ mode: 'executor', capacity: null }, kinds)).toBe('workers');
	});

	it('resolves a bound alias to its kind', () => {
		expect(resolveRunTarget({ mode: 'executor', capacity: { alias: 'lab_fleet' } }, kinds)).toBe(
			'runner_group'
		);
		expect(resolveRunTarget({ mode: 'executor', capacity: { alias: 'gpu_limit' } }, kinds)).toBe(
			'limit'
		);
	});

	it('REGRESSION: an empty-alias capacity is AMBIGUOUS → null, not workers', () => {
		// This is the bug: selecting "Runner group"/"Concurrency limit" writes
		// capacity:{alias:''} until a resource is chosen. A pure derivation that
		// returned 'workers' here snapped the selector back and hid the picker.
		expect(resolveRunTarget({ mode: 'executor', capacity: { alias: '' } }, kinds)).toBeNull();
		expect(resolveRunTarget({ mode: 'executor', capacity: {} }, kinds)).toBeNull();
	});

	it('an unresolved alias (resources not loaded / deleted) falls back to limit', () => {
		expect(resolveRunTarget({ mode: 'executor', capacity: { alias: 'ghost' } }, kinds)).toBe(
			'limit'
		);
		expect(
			resolveRunTarget({ mode: 'executor', capacity: { alias: 'lab_fleet' } }, new Map())
		).toBe('limit'); // empty map → unknown kind → provisional limit
	});

	it('undefined value → workers', () => {
		expect(resolveRunTarget(undefined, kinds)).toBe('workers');
	});
});

describe('initialRunTarget', () => {
	it('seeds a bound step as a capacity target (no kindByAlias needed)', () => {
		expect(initialRunTarget({ mode: 'executor', capacity: { alias: 'lab_fleet' } })).toBe('limit');
		expect(initialRunTarget({ mode: 'executor', capacity: { alias: '' } })).toBe('limit');
	});
	it('seeds workers / scheduled correctly', () => {
		expect(initialRunTarget({ mode: 'executor' })).toBe('workers');
		expect(initialRunTarget(undefined)).toBe('workers');
		expect(initialRunTarget({ mode: 'scheduled', jobTemplate: '' })).toBe('scheduled');
	});
});
