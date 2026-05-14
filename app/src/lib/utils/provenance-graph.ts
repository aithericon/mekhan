/**
 * Transforms a flat AncestryNode[] response from the provenance API into
 * XYFlow-compatible nodes and edges for DAG visualization.
 */

import dagre from '@dagrejs/dagre';
import type { Node, Edge } from '@xyflow/svelte';
import type { AncestryNode, CrossNetEdge } from '$lib/api/client';
import type { ProvenanceGraphNode, ProvenanceGraphEdge } from '$lib/types/provenance';

/** Node data stored in the XYFlow node. */
export type ProvenanceNodeData = ProvenanceGraphNode;

const NODE_WIDTH = 280;
const NODE_HEIGHT = 96;

/** Color mapping for effect handlers. */
export const effectColors: Record<string, string> = {
	executor_submit: '#f59e0b', // amber
	human_task: '#8b5cf6', // violet
	catalogue_register: '#10b981', // green
	process_log_metric: '#6b7280', // gray
	process_log_message: '#6b7280',
	process_start: '#3b82f6', // blue
	process_complete: '#3b82f6'
};

/** Color mapping for event types (fallback when no effect_handler). */
export const eventTypeColors: Record<string, string> = {
	TransitionFired: '#3b82f6', // blue
	EffectCompleted: '#f59e0b', // amber
	EffectFailed: '#ef4444', // red
	TokenCreated: '#6b7280', // gray (seeds/signals — differentiated by label)
	TokenBridgedOut: '#f97316' // orange
};

export function getNodeColor(node: ProvenanceGraphNode): string {
	if (node.effect_handler) {
		return effectColors[node.effect_handler] ?? eventTypeColors[node.event_type] ?? '#6b7280';
	}
	return eventTypeColors[node.event_type] ?? '#6b7280';
}

/** Human-readable label for a provenance node. */
export function getNodeLabel(node: ProvenanceGraphNode): string {
	if (node.transition_name) {
		return node.transition_name;
	}
	if (node.event_type === 'TokenCreated') {
		const place =
			node.tokens.find((t) => t.role === 'produced')?.place_id ?? node.place_name;
		if (!place) return 'Token Created';

		// Signal injection — format the place name nicely
		if (place.startsWith('sig_')) {
			const signal = place
				.replace(/^sig_/, '')
				.replace(/_/g, ' ')
				.replace(/\b\w/g, (c) => c.toUpperCase());
			return `Signal: ${signal}`;
		}

		// Bridge ingress — token arrived from another net
		if (place.endsWith('_inbox') || place === 'exec_queue' || place === 'job_queue') {
			const name = place
				.replace(/_/g, ' ')
				.replace(/\b\w/g, (c) => c.toUpperCase());
			return `Received: ${name}`;
		}

		// Seed / initial token
		const name = place
			.replace(/_/g, ' ')
			.replace(/\b\w/g, (c) => c.toUpperCase());
		return `Init: ${name}`;
	}
	if (node.event_type === 'TokenBridgedOut') {
		return `Bridge → ${node.place_name ?? '?'}`;
	}
	return node.event_type;
}

/**
 * Convert flat ancestry nodes into deduplicated graph nodes and edges.
 *
 * The ancestry response may contain the same (net_id, event_seq) multiple
 * times for different tokens. We group by event to create one graph node
 * per event, then derive edges from consumed→produced token relationships.
 */
/** Signal places that are breadcrumb noise (metrics, logs, artifacts, progress). */
const BREADCRUMB_PLACES = new Set([
	'sig_metric',
	'sig_log',
	'sig_progress',
	'sig_output',
	'sig_phase',
	'metric_log',
	'message_log',
	'progress_log'
]);

/** Effect handlers that are breadcrumb projections, not structural transitions.
 *  Note: catalogue_register is NOT a breadcrumb in provenance view — it's the
 *  root of the chain when tracing from an artifact. */
const BREADCRUMB_HANDLERS = new Set([
	'process_log_metric',
	'process_log_message'
]);

/** Signal places that should NOT be collapsed (they're structural for provenance). */
const KEEP_SIGNAL_PLACES = new Set(['sig_artifact', 'sig_completed']);

/**
 * Filter ancestry to remove noise nodes:
 * 1. Breadcrumb signal injections (metrics, logs, artifacts, progress)
 * 2. Breadcrumb effect handlers (process_log_*, catalogue_register)
 * 3. Signal TokenCreated events whose token is consumed by a transition
 *    already in the graph — the transition shows the signal place in its
 *    consumed list, so the standalone signal node is redundant.
 */
export function filterBreadcrumbs(ancestry: AncestryNode[]): AncestryNode[] {
	// First pass: identify tokens consumed by transitions
	const consumedTokenIds = new Set<string>();
	for (const n of ancestry) {
		if (n.role === 'consumed' && n.event_type !== 'TokenCreated') {
			consumedTokenIds.add(n.token_id);
		}
	}

	return ancestry.filter((n) => {
		// Never filter the root event (depth 0) — it's the target artifact
		if (n.depth === 0) return true;

		// Remove TokenCreated on breadcrumb places
		if (n.event_type === 'TokenCreated' && BREADCRUMB_PLACES.has(n.place_id)) {
			return false;
		}
		// Remove breadcrumb effect handlers (but keep depth 0)
		if (n.effect_handler && BREADCRUMB_HANDLERS.has(n.effect_handler)) {
			return false;
		}
		// Remove signal TokenCreated whose produced token is consumed by
		// a transition — the transition node already shows it.
		// Keep sig_artifact — it's structural for provenance (links the
		// catalogue_register root to the executor chain).
		if (
			n.event_type === 'TokenCreated' &&
			n.role === 'produced' &&
			n.place_id.startsWith('sig_') &&
			!KEEP_SIGNAL_PLACES.has(n.place_id) &&
			consumedTokenIds.has(n.token_id)
		) {
			return false;
		}
		return true;
	});
}

export function buildProvenanceGraph(
	ancestry: AncestryNode[],
	includeBreadcrumbs = false,
	crossNetEdges: CrossNetEdge[] = []
): {
	nodes: ProvenanceGraphNode[];
	edges: ProvenanceGraphEdge[];
} {
	const filtered = includeBreadcrumbs ? ancestry : filterBreadcrumbs(ancestry);

	// Group by event (net_id:event_seq)
	const eventMap = new Map<string, ProvenanceGraphNode>();

	for (const node of filtered) {
		const id = `${node.net_id}:${node.event_seq}`;
		if (!eventMap.has(id)) {
			eventMap.set(id, {
				id,
				event_type: node.event_type,
				transition_name: node.transition_name ?? null,
				effect_handler: node.effect_handler ?? null,
				net_id: node.net_id,
				event_seq: node.event_seq,
				timestamp: node.timestamp,
				place_name: node.place_name ?? null,
				tokens: []
			});
		}
		eventMap.get(id)!.tokens.push({
			token_id: node.token_id,
			role: node.role,
			place_id: node.place_id,
			place_name: node.place_name ?? null
		});
	}

	const nodes = Array.from(eventMap.values());

	// Build a token→producing-event index
	const tokenProducedBy = new Map<string, string>();
	for (const node of nodes) {
		for (const t of node.tokens) {
			if (t.role === 'produced') {
				tokenProducedBy.set(t.token_id, node.id);
			}
		}
	}

	// Derive edges: if event B consumed token T, and event A produced T → edge A→B
	const edgeSet = new Set<string>();
	const edges: ProvenanceGraphEdge[] = [];

	for (const node of nodes) {
		for (const t of node.tokens) {
			if (t.role === 'consumed') {
				const sourceId = tokenProducedBy.get(t.token_id);
				if (sourceId && sourceId !== node.id) {
					const edgeId = `${sourceId}->${node.id}`;
					if (!edgeSet.has(edgeId)) {
						edgeSet.add(edgeId);
						const sourceNode = eventMap.get(sourceId);
						edges.push({
							id: edgeId,
							source: sourceId,
							target: node.id,
							cross_net: sourceNode?.net_id !== node.net_id
						});
					}
				}
			}
		}
	}

	// Cross-net edges from explicit backend data (causality_cross_links).
	// Each edge connects an egress event (source net) to an ingress event (target net).
	for (const cne of crossNetEdges) {
		const sourceId = `${cne.egress_net}:${cne.egress_seq}`;
		const targetId = `${cne.ingress_net}:${cne.ingress_seq}`;

		// Only add if both events are in our filtered graph
		if (!eventMap.has(sourceId) || !eventMap.has(targetId)) continue;

		const edgeId = `cross:${sourceId}->${targetId}`;
		if (!edgeSet.has(edgeId)) {
			edgeSet.add(edgeId);
			edges.push({
				id: edgeId,
				source: sourceId,
				target: targetId,
				cross_net: true
			});
		}
	}

	// Final pass: connect disconnected components within the same net.
	// Signal-injected tokens (from executor watcher) have no process tags
	// and no cross-links, so they form isolated sub-graphs. Connect them
	// to the nearest earlier event in the same net by timestamp.
	const connected = new Set<string>();
	for (const e of edges) {
		connected.add(e.source);
		connected.add(e.target);
	}

	const disconnected = nodes.filter((n) => !connected.has(n.id));
	if (disconnected.length > 0) {
		const byNet = new Map<string, ProvenanceGraphNode[]>();
		for (const n of nodes) {
			if (!byNet.has(n.net_id)) byNet.set(n.net_id, []);
			byNet.get(n.net_id)!.push(n);
		}
		for (const arr of byNet.values()) {
			arr.sort((a, b) => a.event_seq - b.event_seq);
		}

		for (const orphan of disconnected) {
			const sameNet = byNet.get(orphan.net_id);
			if (!sameNet) continue;

			// Find the nearest earlier connected node in the same net
			let best: ProvenanceGraphNode | null = null;
			for (const c of sameNet) {
				if (c.id === orphan.id || c.event_seq >= orphan.event_seq) continue;
				if (!connected.has(c.id)) continue;
				if (!best || c.event_seq > best.event_seq) best = c;
			}

			if (best) {
				const edgeId = `temporal:${best.id}->${orphan.id}`;
				if (!edgeSet.has(edgeId)) {
					edgeSet.add(edgeId);
					edges.push({ id: edgeId, source: best.id, target: orphan.id, cross_net: false });
					connected.add(orphan.id);
				}
			}
		}
	}

	return { nodes, edges };
}

/**
 * Apply dagre layout to the provenance graph nodes and edges.
 * Returns XYFlow-compatible Node[] and Edge[].
 */
export function layoutProvenanceGraph(
	graphNodes: ProvenanceGraphNode[],
	graphEdges: ProvenanceGraphEdge[]
): { nodes: Node[]; edges: Edge[] } {
	const g = new dagre.graphlib.Graph();
	g.setDefaultEdgeLabel(() => ({}));
	g.setGraph({ rankdir: 'TB', nodesep: 30, ranksep: 50 });

	for (const node of graphNodes) {
		g.setNode(node.id, { width: NODE_WIDTH, height: NODE_HEIGHT });
	}
	for (const edge of graphEdges) {
		g.setEdge(edge.source, edge.target);
	}

	dagre.layout(g);

	const nodes: Node[] = graphNodes.map((gn) => {
		const pos = g.node(gn.id);
		return {
			id: gn.id,
			type: 'causality',
			position: { x: pos.x - NODE_WIDTH / 2, y: pos.y - NODE_HEIGHT / 2 },
			data: gn as unknown as Record<string, unknown>
		};
	});

	const edges: Edge[] = graphEdges.map((ge) => ({
		id: ge.id,
		source: ge.source,
		target: ge.target,
		type: 'smoothstep',
		animated: ge.cross_net
	}));

	return { nodes, edges };
}
