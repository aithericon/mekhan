import type { CatalogueEntry } from './catalogue';

/** A single node in the provenance ancestry chain. */
export interface AncestryNode {
	depth: number;
	net_id: string;
	event_seq: number;
	event_type: string;
	token_id: string;
	role: string;
	place_id: string;
	place_name: string | null;
	transition_name: string | null;
	effect_handler: string | null;
	timestamp: string;
}

/** Token participation in an event. */
export interface TokenInfo {
	token_id: string;
	role: string;
	place_id: string;
	place_name: string | null;
	/** Full token payload. Populated for produced/read roles; falls back to
	 *  the producer's payload for consumed tokens when available. */
	data: unknown | null;
}

/** Target coordinates for a `TokenBridgedOut` event. */
export interface BridgeTarget {
	target_net: string;
	target_place: string;
}

/** Dispatch event that originated a signal-injected TokenCreated. */
export interface SignalDispatch {
	dispatch_net: string;
	dispatch_seq: number;
	signal_key: string;
}

/** Rich detail for a single causality event, joining domain tables. */
export interface EventDetail {
	net_id: string;
	event_seq: number;
	event_type: string;
	transition_name: string | null;
	effect_handler: string | null;
	timestamp: string;
	tokens: TokenInfo[];
	task: HpiTaskSummary | null;
	artifact: CatalogueEntry | null;
	metrics: HpiMetricEntry[];
	logs: HpiLogEntry[];
	/** Raw JSON returned by the handler (EffectCompleted) or a failure envelope (EffectFailed). */
	effect_result: unknown | null;
	/** Present only for `TokenBridgedOut`. */
	bridge: BridgeTarget | null;
	/** Present when this event is a signal-injected TokenCreated with a known dispatcher. */
	signal_dispatch: SignalDispatch | null;
}

export interface HpiTaskSummary {
	id: string;
	process_id: string;
	title: string;
	status: string;
	assignee: string | null;
	detail: Record<string, unknown>;
	created_at: string;
	completed_at: string | null;
}

export interface HpiMetricEntry {
	process_id: string;
	key: string;
	value: number;
	timestamp: string;
}

export interface HpiLogEntry {
	id: number;
	process_id: string;
	level: string;
	source: string | null;
	message: string;
	detail: Record<string, unknown>;
	timestamp: string;
}

/** Cross-net bridge link. */
export interface CrossLink {
	signal_key: string;
	egress_net: string | null;
	egress_seq: number | null;
	ingress_net: string | null;
	ingress_seq: number | null;
	link_type: string;
}

/** Explicit cross-net edge from the backend. */
export interface CrossNetEdge {
	signal_key: string;
	egress_net: string;
	egress_seq: number;
	ingress_net: string;
	ingress_seq: number;
	link_type: string;
}

/** Full provenance response from the API. */
export interface ProvenanceResponse {
	nodes: AncestryNode[];
	cross_net_edges: CrossNetEdge[];
}

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
