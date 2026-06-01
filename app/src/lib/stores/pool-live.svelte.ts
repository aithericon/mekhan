/**
 * Pool-live store: live polling of the resource-pool-net marking.
 *
 * Strategy:
 * - On init: fetch the full event log from `/petri/api/nets/{netId}/events`
 *   and the topology from `/petri/api/nets/{netId}/topology` to resolve place
 *   ids from their well-known names.
 * - Poll every POLL_INTERVAL_MS for new events (`from_sequence`).
 * - Fold events through `projectMarking` (the same pure reducer used by the
 *   Petri workbench) to derive the current token distribution.
 * - Expose `poolCount`, `inUseCount`, `capacity`, `holds`, and a conservation
 *   indicator so the PoolContentionView never does the math itself.
 *
 * Net-state endpoint used:
 *   GET /petri/api/nets/{netId}/events[?from_sequence=N]
 *   GET /petri/api/nets/{netId}/topology
 *
 * Both are forwarded through mekhan's `/petri/*` reverse proxy (auth-gated,
 * same session cookie as the rest of the API).
 */

import { projectMarking } from '$lib/stores/petri-projection';
import type { PersistedEvent, PetriNet, Token } from '$lib/types/petri';

export const POOL_NET_ID = 'resource-pool-net';

// Well-known place IDs shared by every pool/adapter backend. Resolve by ID,
// NOT display name: the token-pool builder labels them "Capacity Pool" /
// "Freed Units" and the datacenter adapter labels `done` "Released Leases", but
// the place *ids* (`pool` / `in_use` / `done`) are identical across backends
// (service/src/petri/pool_net.rs; engine/sdk/examples/resource_pool_net.rs). The
// `pool` place is absent on the datacenter adapter (no in-net capacity — the
// external allocator owns it), so `poolCount` falls to 0 there and the view
// degrades to an active-leases list. Name fallbacks kept for the legacy
// prototype net whose ids may differ.
const PLACE_ID_POOL = 'pool';
const PLACE_ID_IN_USE = 'in_use';
const PLACE_ID_DONE = 'done';
const PLACE_NAME_POOL = 'GPU Pool';
const PLACE_NAME_IN_USE = 'In Use';
const PLACE_NAME_DONE = 'Freed Units';

const POLL_INTERVAL_MS = 2_000;
const PETRI_BASE = '/petri';

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/** A single active hold: one token in `in_use`.
 *
 * The hold is backend-agnostic: the `in_use` token carries a typed lease whose
 * shape depends on the pool's resource kind (`token_pool` → `{ unit_id }`;
 * `datacenter` → `{ alloc_id, node?, expiry?, scheduler }`). We surface `grantId`
 * (the correlation key, present on every backend) plus a generic `fields` map
 * of the remaining scalar lease fields so the view never hard-codes a field. */
export interface HoldRecord {
	tokenId: string;
	/** Grant id carried on the token color data, or null. */
	grantId: string | null;
	/** All other scalar lease fields (kind-specific), stringified for display.
	 *  e.g. `{ unit_id }` for a token pool, `{ alloc_id, node, expiry }`
	 *  for a datacenter lease. Empty for Unit tokens. */
	fields: Record<string, string>;
}

export type PoolLiveStatus = 'idle' | 'loading' | 'live' | 'error' | 'net-not-found';

// ---------------------------------------------------------------------------
// Store factory
// ---------------------------------------------------------------------------

export function createPoolLiveStore(netId: string = POOL_NET_ID) {
	const apiBase = `${PETRI_BASE}/api/nets/${netId}`;

	// ── Core state ────────────────────────────────────────────────────────
	let events = $state<PersistedEvent[]>([]);
	let topology = $state<PetriNet | null>(null);
	let status = $state<PoolLiveStatus>('idle');
	let errorMessage = $state<string | null>(null);
	let lastFetchedSequence = 0;
	let pollTimer: ReturnType<typeof setInterval> | null = null;
	let destroyed = false;

	// ── Place id resolution (from topology) ──────────────────────────────
	// Match by the shared place ID first; fall back to the legacy prototype's
	// display name so the standalone example net still resolves.
	function resolvePlaceId(id: string, legacyName: string): string | null {
		if (!topology) return null;
		const byId = topology.places.find((p) => p.id === id);
		if (byId) return byId.id;
		return topology.places.find((p) => p.name === legacyName)?.id ?? null;
	}

	const poolPlaceId = $derived(resolvePlaceId(PLACE_ID_POOL, PLACE_NAME_POOL));
	const inUsePlaceId = $derived(resolvePlaceId(PLACE_ID_IN_USE, PLACE_NAME_IN_USE));
	const donePlaceId = $derived(resolvePlaceId(PLACE_ID_DONE, PLACE_NAME_DONE));

	// ── Projected marking from events ─────────────────────────────────────
	const marking = $derived.by(() => projectMarking(events, events.length - 1));

	// ── Derived pool counts ───────────────────────────────────────────────
	const poolTokens = $derived.by((): Token[] => {
		const id = poolPlaceId;
		if (!id) return [];
		return marking.get(id) ?? [];
	});

	const inUseTokens = $derived.by((): Token[] => {
		const id = inUsePlaceId;
		if (!id) return [];
		return marking.get(id) ?? [];
	});

	const doneTokens = $derived.by((): Token[] => {
		const id = donePlaceId;
		if (!id) return [];
		return marking.get(id) ?? [];
	});

	/** Free capacity count. */
	const poolCount = $derived(poolTokens.length);

	/** In-use (held) capacity count. */
	const inUseCount = $derived(inUseTokens.length);

	/**
	 * Current token sum across pool + in_use.
	 * Conservation holds (pool+in_use == N) when the net is quiescent.
	 * Mid-flight states (claim bridging, not yet registered) can transiently
	 * lower this; the indicator shows a check only when sum > 0 and stable.
	 */
	const capacity = $derived(poolCount + inUseCount);

	/**
	 * Conservation indicator: true when we have a non-zero sum (pool is seeded
	 * and at least some tokens have been accounted for). The Petri net invariant
	 * pool+in_use == N is proven at the engine level; the UI indicator confirms
	 * the marking projection has loaded and reflects a non-empty pool.
	 */
	const conservationOk = $derived(capacity > 0);

	/** Per-hold details from in_use tokens. Backend-agnostic: pulls `grant_id`
	 *  as the correlation key and stringifies every other scalar lease field
	 *  into `fields` so the view renders whatever the resource kind put on the
	 *  lease (unit_id / alloc_id / node / expiry / …). */
	const holds = $derived.by((): HoldRecord[] => {
		return inUseTokens.map((tok) => {
			let grantId: string | null = null;
			const fields: Record<string, string> = {};
			if (
				tok.color.type === 'Data' &&
				tok.color.value !== null &&
				typeof tok.color.value === 'object'
			) {
				const data = tok.color.value as Record<string, unknown>;
				for (const [k, v] of Object.entries(data)) {
					if (k === 'grant_id') {
						grantId = typeof v === 'string' ? v : String(v);
						continue;
					}
					// Skip nested objects/arrays — leases are flat scalar records;
					// anything non-scalar is internal bookkeeping, not lease detail.
					if (v === null || typeof v === 'object') continue;
					fields[k] = String(v);
				}
			}
			return { tokenId: tok.id, grantId, fields };
		});
	});

	/** Count of completed (freed) units since the net started. */
	const freedCount = $derived(doneTokens.length);

	// ── Fetch helpers ─────────────────────────────────────────────────────

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
		// Deduplicate by sequence (mirrors petri.svelte.ts pattern).
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
			if (res.status === 404) {
				status = 'net-not-found';
				stopPolling();
				return;
			}
			// Non-fatal: log and retry next cycle.
			console.warn(`[pool-live] poll failed ${res.status}`);
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

	// ── Polling lifecycle ─────────────────────────────────────────────────

	function startPolling() {
		if (pollTimer !== null) return;
		pollTimer = setInterval(() => {
			if (!destroyed) {
				fetchNewEvents().catch((e) =>
					console.warn('[pool-live] poll error', e)
				);
			}
		}, POLL_INTERVAL_MS);
	}

	function stopPolling() {
		if (pollTimer !== null) {
			clearInterval(pollTimer);
			pollTimer = null;
		}
	}

	async function init() {
		if (destroyed) return;
		status = 'loading';
		errorMessage = null;
		try {
			await fetchTopology();
			// fetchTopology sets status = 'net-not-found' on 404; bail early.
			if ((status as string) === 'net-not-found') return;
			await fetchAllEvents();
			if ((status as string) === 'net-not-found') return;
			status = 'live';
			startPolling();
		} catch (e) {
			errorMessage = e instanceof Error ? e.message : String(e);
			status = 'error';
		}
	}

	function refresh() {
		stopPolling();
		lastFetchedSequence = 0;
		events = [];
		topology = null;
		init();
	}

	function destroy() {
		destroyed = true;
		stopPolling();
	}

	return {
		get status() {
			return status;
		},
		get error() {
			return errorMessage;
		},
		get poolCount() {
			return poolCount;
		},
		get inUseCount() {
			return inUseCount;
		},
		get capacity() {
			return capacity;
		},
		get conservationOk() {
			return conservationOk;
		},
		get holds() {
			return holds;
		},
		get freedCount() {
			return freedCount;
		},
		get eventCount() {
			return events.length;
		},
		init,
		refresh,
		destroy
	};
}

export type PoolLiveStore = ReturnType<typeof createPoolLiveStore>;
