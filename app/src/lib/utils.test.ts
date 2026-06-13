import { describe, it, expect } from 'vitest';
import { timeAgo, instanceIdFromNet, instanceIdFromExecution } from './utils';

describe('timeAgo', () => {
	const now = new Date('2026-06-11T12:00:00Z');

	it('returns empty string for null/invalid', () => {
		expect(timeAgo(null, now)).toBe('');
		expect(timeAgo(undefined, now)).toBe('');
		expect(timeAgo('not-a-date', now)).toBe('');
	});

	it('treats sub-45s and future timestamps as "just now"', () => {
		expect(timeAgo(new Date('2026-06-11T11:59:30Z'), now)).toBe('just now');
		expect(timeAgo(new Date('2026-06-11T12:05:00Z'), now)).toBe('just now'); // clock skew
	});

	it('renders minutes / hours / days', () => {
		expect(timeAgo(new Date('2026-06-11T11:55:00Z'), now)).toBe('5m ago');
		expect(timeAgo(new Date('2026-06-11T09:00:00Z'), now)).toBe('3h ago');
		expect(timeAgo(new Date('2026-06-09T12:00:00Z'), now)).toBe('2d ago');
	});

	it('renders weeks / months / years past the day threshold', () => {
		expect(timeAgo(new Date('2026-05-28T12:00:00Z'), now)).toBe('2w ago');
		expect(timeAgo(new Date('2026-03-11T12:00:00Z'), now)).toBe('3mo ago');
		expect(timeAgo(new Date('2024-06-11T12:00:00Z'), now)).toBe('2y ago');
	});

	it('accepts an ISO string', () => {
		expect(timeAgo('2026-06-11T11:55:00Z', now)).toBe('5m ago');
	});
});

describe('instanceIdFromNet', () => {
	const ws = '11111111-1111-1111-1111-111111111111';
	const inst = '22222222-2222-2222-2222-222222222222';

	it('extracts the instance UUID from a workspace-namespaced net_id', () => {
		expect(instanceIdFromNet(`mekhan-${ws}-${inst}`)).toBe(inst);
	});

	it('extracts from the legacy mekhan-{instance} format', () => {
		expect(instanceIdFromNet(`mekhan-${inst}`)).toBe(inst);
	});

	it('returns null for non-mekhan nets and nullish input', () => {
		expect(instanceIdFromNet('pool-abc')).toBeNull();
		expect(instanceIdFromNet('staging-xyz')).toBeNull();
		expect(instanceIdFromNet(null)).toBeNull();
		expect(instanceIdFromNet(undefined)).toBeNull();
		expect(instanceIdFromNet('mekhan-short')).toBeNull();
	});
});

describe('instanceIdFromExecution', () => {
	const ws = '11111111-1111-1111-1111-111111111111';
	const inst = '22222222-2222-2222-2222-222222222222';
	const run = '33333333-3333-3333-3333-333333333333';

	it('extracts the instance UUID (second segment) from an execution_id', () => {
		expect(instanceIdFromExecution(`mekhan-${ws}-${inst}-${run}`)).toBe(inst);
		// Also correct when fed a bare net_id (ws+inst, no run suffix).
		expect(instanceIdFromExecution(`mekhan-${ws}-${inst}`)).toBe(inst);
	});

	it('returns null when the workspace+instance UUIDs are not both present', () => {
		expect(instanceIdFromExecution(`mekhan-${inst}`)).toBeNull();
		expect(instanceIdFromExecution(null)).toBeNull();
		expect(instanceIdFromExecution('pool-abc')).toBeNull();
	});
});
