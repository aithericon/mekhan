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

	/** Whether the deployed instance net declares this place id at all. */
	function hasPlace(placeId: string): boolean {
		return placeIds.has(placeId);
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
		hasPlace,
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
