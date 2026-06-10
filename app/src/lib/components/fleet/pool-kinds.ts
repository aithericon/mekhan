/**
 * The Fleet page's pool taxonomy — the SINGLE place that maps a capacity's
 * axes/backend onto operator vocabulary. Section order, kind chips, the
 * plain-language axes line on the pool detail band, and empty states all
 * read from here so the Pools list and the pool detail page cannot drift.
 *
 * Domain truth (docs/35): a capacity IS a pool; runners / workers / humans
 * are its members. `backend` is derived from the liveness axis; the consent
 * acceptance axis is what distinguishes a human pool from a machine pool.
 */
import type { CapacitySummary } from '$lib/api/capacities';

export type PoolKindId = 'machine' | 'worker' | 'human' | 'limit' | 'cluster' | 'broken';

export interface PoolKind {
	id: PoolKindId;
	/** Section heading on the Pools tab (plural). */
	label: string;
	/** Short kind chip on a pool row / detail band. */
	chip: string;
	/** Plain-language axes line for the pool detail band. */
	plainAxes: string;
	/** What a member of this pool is called (singular / plural). */
	member: [string, string];
	/** Section empty-state message. */
	empty: string;
}

export const POOL_KINDS: Record<PoolKindId, PoolKind> = {
	machine: {
		id: 'machine',
		label: 'Machine pools',
		chip: 'machines',
		plainAxes: 'Live machines · auto-accept · capacity follows presence',
		member: ['runner', 'runners'],
		empty: 'No machine pools. Create one, then enroll runners into it.'
	},
	worker: {
		id: 'worker',
		label: 'Worker pools',
		chip: 'workers',
		plainAxes: 'Pull workers · auto-accept · queue-balanced',
		member: ['worker', 'workers'],
		empty: 'No worker pools.'
	},
	human: {
		id: 'human',
		label: 'Human pools',
		chip: 'people',
		plainAxes: 'People · claim to accept · capacity follows presence',
		member: ['member', 'members'],
		empty: 'No human pools. Create one with the Human pool kind, then enroll members.'
	},
	limit: {
		id: 'limit',
		label: 'Limits',
		chip: 'limit',
		plainAxes: 'Concurrency semaphore · fixed seats, no members',
		member: ['holder', 'holders'],
		empty: 'No concurrency limits.'
	},
	cluster: {
		id: 'cluster',
		label: 'Clusters',
		chip: 'cluster',
		plainAxes: 'External scheduler · leased allocations',
		member: ['lease', 'leases'],
		empty: 'No scheduler clusters.'
	},
	broken: {
		id: 'broken',
		label: 'Not dispatchable',
		chip: 'broken',
		plainAxes: 'Axes failed to parse — recreate or delete this pool',
		member: ['member', 'members'],
		empty: ''
	}
};

/** Pools tab section order. `broken` only renders when non-empty. */
export const POOL_KIND_ORDER: PoolKindId[] = [
	'machine',
	'worker',
	'human',
	'limit',
	'cluster',
	'broken'
];

/** Classify one capacity row into its operator-facing pool kind. */
export function poolKindOf(c: CapacitySummary): PoolKind {
	switch (c.backend) {
		case 'presence':
			return c.axes?.acceptance === 'consent' ? POOL_KINDS.human : POOL_KINDS.machine;
		case 'queue':
			return POOL_KINDS.worker;
		case 'tokens':
			return POOL_KINDS.limit;
		case 'scheduler':
			return POOL_KINDS.cluster;
		default:
			return POOL_KINDS.broken;
	}
}

/** `online/total`-style live line for a pool row, per kind. */
export function poolLiveLine(c: CapacitySummary): string | null {
	const live = c.live;
	switch (live.kind) {
		case 'presence':
			return `${live.online}/${live.total} online`;
		case 'queue':
			return `${live.online}/${live.enrolled} online`;
		case 'tokens':
			return `${live.in_use}/${live.seeded} in use`;
		case 'scheduler':
			return `${live.active_leases} active ${live.active_leases === 1 ? 'lease' : 'leases'}`;
		default:
			return null;
	}
}
