import { describe, it, expect } from 'vitest';
import { poolKindOf, poolLiveLine } from './pool-kinds';
import type { CapacitySummary, CapacityLive } from '$lib/api/capacities';

// Minimal summary stub — poolKindOf reads `backend` + `axes.acceptance`,
// poolLiveLine reads `live`. Everything else is inert row identity.
function summary(
	backend: CapacitySummary['backend'],
	acceptance?: 'auto' | 'consent',
	live: CapacityLive = { kind: 'none' }
): CapacitySummary {
	return {
		id: 'cap-1',
		path: 'pools.x',
		display_name: 'X',
		backend,
		axes: acceptance ? ({ acceptance } as unknown as CapacitySummary['axes']) : null,
		live
	} as unknown as CapacitySummary;
}

describe('poolKindOf', () => {
	it('presence + auto ⇒ machine', () => {
		expect(poolKindOf(summary('presence', 'auto')).id).toBe('machine');
	});

	it('presence + consent ⇒ human', () => {
		expect(poolKindOf(summary('presence', 'consent')).id).toBe('human');
	});

	it('presence with no axes ⇒ machine', () => {
		expect(poolKindOf(summary('presence')).id).toBe('machine');
	});

	it('queue ⇒ worker', () => {
		expect(poolKindOf(summary('queue')).id).toBe('worker');
	});

	it('tokens ⇒ limit', () => {
		expect(poolKindOf(summary('tokens')).id).toBe('limit');
	});

	it('scheduler ⇒ cluster', () => {
		expect(poolKindOf(summary('scheduler')).id).toBe('cluster');
	});

	it('null backend (axes failed to parse) ⇒ broken', () => {
		expect(poolKindOf(summary(null)).id).toBe('broken');
	});
});

describe('poolLiveLine', () => {
	it('presence ⇒ online/total', () => {
		const c = summary('presence', 'auto', { kind: 'presence', online: 2, total: 3, backends: [] });
		expect(poolLiveLine(c)).toBe('2/3 online');
	});

	it('queue ⇒ online/enrolled', () => {
		const c = summary('queue', undefined, { kind: 'queue', online: 1, enrolled: 4, backends: [] });
		expect(poolLiveLine(c)).toBe('1/4 online');
	});

	it('tokens ⇒ in_use/seeded', () => {
		const c = summary('tokens', undefined, { kind: 'tokens', in_use: 2, seeded: 5, holders: [] });
		expect(poolLiveLine(c)).toBe('2/5 in use');
	});

	it('scheduler ⇒ active leases, singular', () => {
		const c = summary('scheduler', undefined, {
			kind: 'scheduler',
			active_leases: 1,
			draining: false,
			flavor: 'nomad',
			watcher_state: 'connected'
		});
		expect(poolLiveLine(c)).toBe('1 active lease');
	});

	it('scheduler ⇒ active leases, plural', () => {
		const c = summary('scheduler', undefined, {
			kind: 'scheduler',
			active_leases: 3,
			draining: true,
			flavor: 'slurm',
			watcher_state: 'connected'
		});
		expect(poolLiveLine(c)).toBe('3 active leases');
	});

	it('none ⇒ null', () => {
		expect(poolLiveLine(summary(null))).toBeNull();
	});
});
