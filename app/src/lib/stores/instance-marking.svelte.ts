/**
 * Instance-net marking store: a thin reader of one workflow instance's live
 * Petri-net marking, used to drive per-node runtime overlays that need token
 * counts (not just step-execution rows).
 *
 * It reuses the EXACT same engine source the pool-live store uses —
 *   GET /petri/api/nets/{netId}/events[?from_sequence=N]
 *   GET /petri/api/nets/{netId}/topology
 * forwarded through mekhan's `/petri/*` reverse proxy — and folds events with
 * the same pure `projectMarking` reducer as the Petri workbench, so the count
 * math never drifts from the workbench / pool view.
 *
 * Deliberately poll-less: this store does NOT own a timer. The caller
 * (`WorkflowGraphView`) already polls step executions every 2 s; it drives
 * `refresh()` from that same cycle so the instance view keeps a single poll.
 * `init()` does the one-time topology + full-log fetch; `refresh()` pulls only
 * new events (`from_sequence`) and re-folds.
 *
 * The M3 resource-pool "waiting for resource" predicate is computed here via
 * `count(placeId)` against the instance marking — see
 * `WorkflowGraphView.svelte` for the predicate and the bridge-out caveat that
 * drives the choice of `p_{id}_pending` / `p_{id}_held` over `claim_out` /
 * `grant_inbox`.
 */

import { projectMarking } from '$lib/stores/petri-projection';
import type { PersistedEvent, PetriNet, Token } from '$lib/types/petri';

const PETRI_BASE = '/petri';

export type InstanceMarkingStatus = 'idle' | 'loading' | 'live' | 'error' | 'net-not-found';

export function createInstanceMarkingStore(netId: string) {
	const apiBase = `${PETRI_BASE}/api/nets/${netId}`;

	let events = $state<PersistedEvent[]>([]);
	let topology = $state<PetriNet | null>(null);
	let status = $state<InstanceMarkingStatus>('idle');
	let errorMessage = $state<string | null>(null);
	let lastFetchedSequence = 0;
	let destroyed = false;

	// place id → tokens, folded from the event log (same reducer as the
	// workbench). `events.length - 1` == live tail.
	const marking = $derived.by(() => projectMarking(events, events.length - 1));

	// Set of place ids present in the deployed topology, so a count() lookup
	// can tell "place exists, currently 0 tokens" from "place not in this net".
	const placeIds = $derived.by(() => {
		const s = new Set<string>();
		if (topology) for (const p of topology.places) s.add(p.id);
		return s;
	});

	/** Token count parked in a place id (0 when absent or unknown). */
	function count(placeId: string): number {
		const toks: Token[] = marking.get(placeId) ?? [];
		return toks.length;
	}

	/** The color data of the first `Data` token parked in a place (null when the
	 *  place is empty, absent, or holds a non-Data token). Used to surface the
	 *  parked lease envelope (`p_{scope}_data` → `{ lease, … }`) in the drawer. */
	function tokenData(placeId: string): Record<string, unknown> | null {
		const toks: Token[] = marking.get(placeId) ?? [];
		for (const t of toks) {
			if (t.color?.type === 'Data' && t.color.value !== null && typeof t.color.value === 'object') {
				return t.color.value as Record<string, unknown>;
			}
		}
		return null;
	}

	/** Whether the deployed instance net declares this place id at all. */
	function hasPlace(placeId: string): boolean {
		return placeIds.has(placeId);
	}

	/** The target net id a `bridge_out` place forwards to (e.g. a pooled node's
	 *  `p_{id}_claim_out` → `pool-<resource_id>`). Lets the pool overlay resolve
	 *  the REAL backing-net id from the deployed topology instead of guessing.
	 *  Null when the place is absent or not a bridge_out. */
	function bridgeTarget(placeId: string): string | null {
		const p = topology?.places.find((pl) => pl.id === placeId);
		return p?.bridge_target?.target_net_id ?? p?.target_net_id ?? null;
	}

	async function fetchTopology(): Promise<void> {
		const res = await fetch(`${apiBase}/topology`);
		if (!res.ok) {
			if (res.status === 404) {
				status = 'net-not-found';
				return;
			}
			throw new Error(`Topology fetch ${res.status}: ${await res.text().catch(() => '')}`);
		}
		const data: { topology?: PetriNet } = await res.json();
		topology = (data.topology ?? data) as PetriNet;
	}

	async function fetchAllEvents(): Promise<void> {
		const res = await fetch(`${apiBase}/events`);
		if (!res.ok) {
			if (res.status === 404) {
				status = 'net-not-found';
				return;
			}
			throw new Error(`Events fetch ${res.status}: ${await res.text().catch(() => '')}`);
		}
		const data: { events?: PersistedEvent[] } = await res.json();
		const raw = data.events ?? [];
		const seen = new Set<number>();
		events = raw.filter((e) => {
			if (seen.has(e.sequence)) return false;
			seen.add(e.sequence);
			return true;
		});
		if (events.length > 0) {
			lastFetchedSequence = events[events.length - 1].sequence;
		}
	}

	async function fetchNewEvents(): Promise<void> {
		const res = await fetch(`${apiBase}/events?from_sequence=${lastFetchedSequence + 1}`);
		if (!res.ok) {
			// Net tombstoned (409) or transiently unavailable — keep the last
			// marking; the badge just stops updating. 404 means the net is gone.
			if (res.status === 404) status = 'net-not-found';
			return;
		}
		const data: { events?: PersistedEvent[] } = await res.json();
		const newEvents = data.events ?? [];
		if (newEvents.length > 0) {
			const existingSeqs = new Set(events.map((e) => e.sequence));
			const unique = newEvents.filter((e) => !existingSeqs.has(e.sequence));
			if (unique.length > 0) {
				events = [...events, ...unique];
				lastFetchedSequence = events[events.length - 1].sequence;
			}
		}
	}

	/** One-time topology + full-log load. Idempotent-safe to call once. */
	async function init(): Promise<void> {
		if (destroyed) return;
		status = 'loading';
		errorMessage = null;
		try {
			await fetchTopology();
			if ((status as string) === 'net-not-found') return;
			await fetchAllEvents();
			if ((status as string) === 'net-not-found') return;
			status = 'live';
		} catch (e) {
			errorMessage = e instanceof Error ? e.message : String(e);
			status = 'error';
		}
	}

	/**
	 * Incremental refresh — pull only new events. Driven by the caller's
	 * existing poll cycle (no internal timer). No-op until `init()` resolved a
	 * topology. Falls back to a full `init()` if the net wasn't loaded yet.
	 */
	async function refresh(): Promise<void> {
		if (destroyed) return;
		if (topology === null) {
			await init();
			return;
		}
		try {
			await fetchNewEvents();
		} catch {
			// Non-fatal: keep the current marking, retry next cycle.
		}
	}

	function destroy(): void {
		destroyed = true;
	}

	return {
		get status() {
			return status;
		},
		get error() {
			return errorMessage;
		},
		get eventCount() {
			return events.length;
		},
		count,
		tokenData,
		hasPlace,
		bridgeTarget,
		init,
		refresh,
		destroy
	};
}

export type InstanceMarkingStore = ReturnType<typeof createInstanceMarkingStore>;

// ---------------------------------------------------------------------------
// M3 resource-pool predicate helpers
// ---------------------------------------------------------------------------

/**
 * "Waiting for resource" predicate for a resource-pool AutomatedStep, computed
 * against an instance-net marking.
 *
 * We use `count(p_{id}_pending) > 0 AND count(p_{id}_held) == 0` — NOT
 * `claim_out` / `grant_inbox`:
 *
 *   - `p_{id}_claim_out` is a `bridge_out` place. `projectMarking` removes a
 *     token from its source place on `TokenBridgedOut`, so the claim token is
 *     gone from `claim_out` the instant it's sent to the pool — it never
 *     "parks" there, so its count is ~always 0 and useless as a wait signal.
 *   - `p_{id}_pending` is a `state` place that `t_{id}_claim` parks
 *     {input, grant_id} into and `t_{id}_acquire` consumes once the grant
 *     arrives. `p_{id}_held` is a `state` place produced only by
 *     `t_{id}_acquire`. So `pending>0 && held==0` is exactly "claim emitted,
 *     grant not yet acquired" — and both places persist in the marking.
 */
export function isAwaitingResource(store: InstanceMarkingStore, nodeId: string): boolean {
	const pending = `p_${nodeId}_pending`;
	const held = `p_${nodeId}_held`;
	// Only meaningful for pooled nodes — gate on the place existing so
	// non-pool nodes never light up.
	if (!store.hasPlace(pending)) return false;
	return store.count(pending) > 0 && store.count(held) === 0;
}

// ---------------------------------------------------------------------------
// Cluster lease runtime — per-LeaseScope lifecycle + typed lease detail
// ---------------------------------------------------------------------------

/** Lifecycle of a LeaseScope's datacenter allocation, derived from the
 *  instance net marking. */
export type LeaseState = 'idle' | 'claiming' | 'held' | 'released' | 'failed';

/** The typed lease the engine parked (the `DatacenterLease` shape + grant_id),
 *  surfaced generically so the view never hard-codes a flavor's fields. */
export interface LeaseRuntime {
	state: LeaseState;
	/** alloc_id, node, expiry, executor_namespace — present once acquired. */
	allocId: string | null;
	node: string | null;
	expiry: string | null;
	executorNamespace: string | null;
	/** Scheduler flavor (`scheduler.flavor`) when acquired. */
	flavor: string | null;
	/** Remaining scheduler-specific detail (`scheduler.<field>` minus flavor),
	 *  stringified for display — e.g. `{ partition }` (slurm), `{ eval_id }`
	 *  (nomad). Empty for http. */
	schedulerDetail: Record<string, string>;
}

/** String-coerce a scalar JSON value for display; null for non-scalars. */
function asScalar(v: unknown): string | null {
	if (v === null || v === undefined) return null;
	if (typeof v === 'object') return null;
	return String(v);
}

/**
 * Read a LeaseScope's lease lifecycle + typed detail from the instance marking.
 *
 * Places (from `emit_lease_bridge`): `p_{id}_pending` (claim parked),
 * `p_{id}_held` (hold), `p_{id}_data` (the parked `{ lease, … }` envelope),
 * `p_{id}_lease_failed_parked` (held-alloc death). The lease detail lives under
 * the envelope's `lease` key once acquired. Returns null when the node isn't a
 * lease holder in this net (no `p_{id}_data`).
 */
export function leaseRuntimeFor(store: InstanceMarkingStore, nodeId: string): LeaseRuntime | null {
	const dataPlace = `p_${nodeId}_data`;
	const heldPlace = `p_${nodeId}_held`;
	const pendingPlace = `p_${nodeId}_pending`;
	const failedPlace = `p_${nodeId}_lease_failed_parked`;
	// A lease holder always declares these places; bail otherwise so non-lease
	// nodes never produce a (misleading) lease panel.
	if (!store.hasPlace(dataPlace) || !store.hasPlace(heldPlace)) return null;

	const envelope = store.tokenData(dataPlace);
	const lease =
		envelope && typeof envelope.lease === 'object' && envelope.lease !== null
			? (envelope.lease as Record<string, unknown>)
			: null;

	let state: LeaseState;
	if (store.count(failedPlace) > 0) state = 'failed';
	else if (store.count(heldPlace) > 0) state = 'held';
	else if (lease) state = 'released'; // acquired then exited (held consumed)
	else if (store.count(pendingPlace) > 0) state = 'claiming';
	else state = 'idle';

	const scheduler =
		lease && typeof lease.scheduler === 'object' && lease.scheduler !== null
			? (lease.scheduler as Record<string, unknown>)
			: null;
	const schedulerDetail: Record<string, string> = {};
	if (scheduler) {
		for (const [k, v] of Object.entries(scheduler)) {
			if (k === 'flavor') continue;
			const s = asScalar(v);
			if (s !== null) schedulerDetail[k] = s;
		}
	}

	return {
		state,
		allocId: lease ? asScalar(lease.alloc_id) : null,
		node: lease ? asScalar(lease.node) : null,
		expiry: lease ? asScalar(lease.expiry) : null,
		executorNamespace: lease ? asScalar(lease.executor_namespace) : null,
		flavor: scheduler ? asScalar(scheduler.flavor) : null,
		schedulerDetail
	};
}
