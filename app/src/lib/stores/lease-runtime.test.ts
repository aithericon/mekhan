import { describe, it, expect } from 'vitest';
import { leaseRuntimeFor, type InstanceMarkingStore } from './instance-marking.svelte';

/**
 * `leaseRuntimeFor` reads a LeaseScope's lifecycle + typed lease detail from the
 * instance marking. These tests feed a fake store whose places mirror the REAL
 * envelope the Nomad lease e2e parks at `p_<scope>_data` (proven live by
 * `scheduled_lease_nomad_e2e`): `{ lease: DatacenterLease, … }`. They pin the
 * reader against the new typed format (alloc_id + per-flavor scheduler detail,
 * no gpu_uuid) without needing a live engine.
 */

const SCOPE = 'ascope';

/** Minimal fake store: only the three accessors leaseRuntimeFor uses. */
function fakeStore(opts: {
	places: Set<string>;
	counts?: Record<string, number>;
	data?: Record<string, Record<string, unknown>>;
}): InstanceMarkingStore {
	return {
		hasPlace: (id: string) => opts.places.has(id),
		count: (id: string) => opts.counts?.[id] ?? 0,
		tokenData: (id: string) => opts.data?.[id] ?? null
	} as unknown as InstanceMarkingStore;
}

const LEASE_PLACES = new Set([`p_${SCOPE}_data`, `p_${SCOPE}_held`]);

/** A held Nomad lease envelope, exactly as the engine parks it. */
const NOMAD_HELD = {
	[`p_${SCOPE}_data`]: {
		lease: {
			grant_id: 'inst-1:ascope',
			alloc_id: 'petri-lease-executor/dispatch-7',
			executor_namespace: 'lease-inst-1-ascope',
			scheduler: { flavor: 'nomad', eval_id: 'e-42' }
		},
		iteration: 2
	}
};

describe('leaseRuntimeFor', () => {
	it('returns null for a non-lease node (no data/held places)', () => {
		const store = fakeStore({ places: new Set(['p_other_data']) });
		expect(leaseRuntimeFor(store, SCOPE)).toBeNull();
	});

	it('reads a held Nomad lease: typed core + per-flavor scheduler detail', () => {
		const store = fakeStore({
			places: LEASE_PLACES,
			counts: { [`p_${SCOPE}_held`]: 1 },
			data: NOMAD_HELD
		});
		const lr = leaseRuntimeFor(store, SCOPE)!;
		expect(lr.state).toBe('held');
		expect(lr.allocId).toBe('petri-lease-executor/dispatch-7');
		expect(lr.executorNamespace).toBe('lease-inst-1-ascope');
		expect(lr.flavor).toBe('nomad');
		// scheduler detail surfaces eval_id (minus the flavor discriminator).
		expect(lr.schedulerDetail).toEqual({ eval_id: 'e-42' });
		// node/expiry omitted by the Nomad allocator → null, not "".
		expect(lr.node).toBeNull();
		expect(lr.expiry).toBeNull();
	});

	it('claiming: pending parked, no grant yet, no lease envelope', () => {
		const store = fakeStore({
			places: LEASE_PLACES,
			counts: { [`p_${SCOPE}_pending`]: 1, [`p_${SCOPE}_held`]: 0 }
			// no data → no lease yet
		});
		const lr = leaseRuntimeFor(store, SCOPE)!;
		expect(lr.state).toBe('claiming');
		expect(lr.allocId).toBeNull();
		expect(lr.flavor).toBeNull();
	});

	it('released: lease acquired then exited (held consumed, envelope retained)', () => {
		const store = fakeStore({
			places: LEASE_PLACES,
			counts: { [`p_${SCOPE}_held`]: 0 },
			data: NOMAD_HELD
		});
		expect(leaseRuntimeFor(store, SCOPE)!.state).toBe('released');
	});

	it('failed: held-allocation-death parked takes precedence', () => {
		const store = fakeStore({
			places: new Set([...LEASE_PLACES, `p_${SCOPE}_lease_failed_parked`]),
			counts: { [`p_${SCOPE}_lease_failed_parked`]: 1, [`p_${SCOPE}_held`]: 1 },
			data: NOMAD_HELD
		});
		expect(leaseRuntimeFor(store, SCOPE)!.state).toBe('failed');
	});

	it('slurm lease surfaces partition; no gpu_uuid anywhere', () => {
		const store = fakeStore({
			places: LEASE_PLACES,
			counts: { [`p_${SCOPE}_held`]: 1 },
			data: {
				[`p_${SCOPE}_data`]: {
					lease: {
						alloc_id: 'job-48213',
						node: 'gpu-node-04',
						executor_namespace: 'lease-x',
						scheduler: { flavor: 'slurm', partition: 'gpu' }
					}
				}
			}
		});
		const lr = leaseRuntimeFor(store, SCOPE)!;
		expect(lr.flavor).toBe('slurm');
		expect(lr.node).toBe('gpu-node-04');
		expect(lr.schedulerDetail).toEqual({ partition: 'gpu' });
		expect(JSON.stringify(lr)).not.toContain('gpu_uuid');
	});
});
