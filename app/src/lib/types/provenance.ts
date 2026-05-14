/**
 * Frontend-only provenance shapes. The wire types
 * (`AncestryNode`, `CrossNetEdge`, `ProvenanceResponse`, `EventDetail`,
 * `CrossLink`, `TokenInfo`, `BridgeTarget`, `SignalDispatch`) are exported
 * from `$lib/api/client`. This file keeps the graph-rendering shapes that the
 * UI derives client-side from those wire types — they have no backend
 * counterpart.
 */

/** A node in the provenance DAG (deduplicated by event). */
export interface ProvenanceGraphNode {
	id: string; // "{net_id}:{event_seq}"
	event_type: string;
	transition_name: string | null;
	effect_handler: string | null;
	net_id: string;
	event_seq: number;
	timestamp: string;
	place_name: string | null;
	tokens: { token_id: string; role: string; place_id: string; place_name: string | null }[];
}

/** An edge in the provenance DAG. */
export interface ProvenanceGraphEdge {
	id: string;
	source: string; // node id (cause)
	target: string; // node id (effect)
	cross_net: boolean;
}
