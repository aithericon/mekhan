/**
 * Cloud-layer visualization helpers for mekhan/app.
 *
 * Typed wrappers for the 4 SSE event types emitted by cloud-layer-workflow's
 * mekhan_visualization module, plus fetch helpers for the BFF proxy endpoints
 * mounted by mekhan/service at /api/cloud-layer/runs/{run_id}/...
 *
 * Auth: all fetches use same-origin paths; the `mekhan_session` HttpOnly cookie
 * is attached automatically by the browser. No Bearer token in browser code.
 */

import type { PetriNet, Token } from '$lib/types/petri';
import { authFetch } from '$lib/auth/fetch';

// ---------------------------------------------------------------------------
// BFF endpoint paths
// ---------------------------------------------------------------------------

export function topologyUrl(runId: string): string {
	return `/api/cloud-layer/runs/${runId}/topology`;
}

export function streamUrl(runId: string): string {
	return `/api/cloud-layer/runs/${runId}/stream`;
}

export function tokenPayloadUrl(runId: string, tokenId: string): string {
	return `/api/cloud-layer/runs/${runId}/tokens/${tokenId}/payload`;
}

// ---------------------------------------------------------------------------
// Topology fetch
// ---------------------------------------------------------------------------

/**
 * Fetch the petri-net topology for a cloud-layer run.
 * Returns the PetriNet (places/transitions/arcs) or throws on error.
 */
export async function fetchCloudLayerTopology(runId: string): Promise<PetriNet> {
	const res = await authFetch(topologyUrl(runId));
	if (!res.ok) {
		throw new Error(`topology fetch failed: ${res.status}`);
	}
	const data = await res.json();
	// The BFF relays cloud-layer's TopologyProxyResponse which flattens the mekhan
	// topology JSON. Mekhan's topology endpoint returns:
	//   { topology: { places, transitions, arcs, groups } }
	// The BFF passes this through with #[serde(flatten)] so the outer JSON is the
	// TopologyProxyResponse. We unwrap the nested topology field if present.
	const net = data.topology ?? data;
	if (!net || !Array.isArray(net.places)) {
		throw new Error('topology response missing places array');
	}
	return net as PetriNet;
}

// ---------------------------------------------------------------------------
// Token payload fetch
// ---------------------------------------------------------------------------

/**
 * Fetch the payload of a specific token from the cloud-layer run.
 */
export async function fetchTokenPayload(
	runId: string,
	tokenId: string
): Promise<TokenPayloadResponse> {
	const res = await authFetch(tokenPayloadUrl(runId, tokenId));
	if (!res.ok) {
		throw new Error(`token payload fetch failed: ${res.status}`);
	}
	return res.json() as Promise<TokenPayloadResponse>;
}

// ---------------------------------------------------------------------------
// SSE event types (4 types — supervisor Q-C approved taxonomy)
// ---------------------------------------------------------------------------

/**
 * `marking_updated` — marking projection changed.
 * Payload: `{ marking: { [place_id]: string[] } }` (place_id → token_id[])
 */
export type MarkingUpdatedEvent = {
	type: 'marking_updated';
	marking: Record<string, string[]>;
};

/**
 * `token_added` — a new token appeared at a place.
 * Payload: `{ place_id, token_id, color }`
 */
export type TokenAddedEvent = {
	type: 'token_added';
	place_id: string;
	token_id: string;
	color: unknown;
};

/**
 * `transition_fired` — a transition completed or failed.
 * Payload: `{ transition_id, outcome, consumed?, produced?, error_message? }`
 */
export type TransitionFiredEvent = {
	type: 'transition_fired';
	transition_id: string;
	outcome: 'completed' | 'failed';
	consumed?: unknown;
	produced?: unknown;
	error_message?: string;
};

/**
 * `artifact_published` — an effect produced an artifact reference.
 * Payload: `{ transition_id, artifact: { artifact_ref?, artifact_url?, content?, ... } }`
 */
export type ArtifactPublishedEvent = {
	type: 'artifact_published';
	transition_id: string;
	artifact: Record<string, unknown>;
};

export type CloudLayerSseEvent =
	| MarkingUpdatedEvent
	| TokenAddedEvent
	| TransitionFiredEvent
	| ArtifactPublishedEvent;

// ---------------------------------------------------------------------------
// Token payload response (mirrors cloud-layer's TokenPayloadResponse)
// ---------------------------------------------------------------------------

export type TokenPayloadResponse = {
	token_color: string;
	value: unknown;
};

// ---------------------------------------------------------------------------
// Cloud-layer marking (derived from marking_updated events)
// ---------------------------------------------------------------------------

/**
 * Convert a cloud-layer marking projection (place_id → token_id[]) to the
 * Map<string, Token[]> shape expected by LabCanvas.
 *
 * Cloud-layer only provides token IDs in the marking projection, not full
 * Token objects (those require a separate /tokens/{id}/payload fetch). We
 * synthesize minimal Token objects from the IDs so LabCanvas can render them.
 */
export function markingProjectionToTokenMap(
	marking: Record<string, string[]>
): Map<string, Token[]> {
	const map = new Map<string, Token[]>();
	for (const [placeId, tokenIds] of Object.entries(marking)) {
		const tokens: Token[] = tokenIds.map((id) => ({
			id,
			color: { type: 'Unit' as const },
			created_at: new Date().toISOString()
		}));
		map.set(placeId, tokens);
	}
	return map;
}

// ---------------------------------------------------------------------------
// SSE subscription
// ---------------------------------------------------------------------------

/**
 * Subscribe to the cloud-layer mekhan-stream SSE for a run.
 *
 * Uses `EventSource` which carries the `mekhan_session` HttpOnly cookie
 * automatically on same-origin requests (no `withCredentials` needed for
 * same-origin per the Fetch spec).
 *
 * Returns an unsubscribe function that closes the EventSource.
 */
export function subscribeToCloudLayerStream(
	runId: string,
	onEvent: (event: CloudLayerSseEvent) => void,
	onError?: (error: Event) => void
): () => void {
	const es = new EventSource(streamUrl(runId));

	function makeListener(eventType: CloudLayerSseEvent['type']) {
		return (e: MessageEvent) => {
			try {
				const data = JSON.parse(e.data) as Record<string, unknown>;
				onEvent({ type: eventType, ...data } as CloudLayerSseEvent);
			} catch {
				// Malformed SSE data — skip
			}
		};
	}

	es.addEventListener('marking_updated', makeListener('marking_updated'));
	es.addEventListener('token_added', makeListener('token_added'));
	es.addEventListener('transition_fired', makeListener('transition_fired'));
	es.addEventListener('artifact_published', makeListener('artifact_published'));

	if (onError) {
		es.onerror = onError;
	}

	return () => es.close();
}
