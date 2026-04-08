/**
 * Transforms a flat AncestryNode[] response from the provenance API into
 * XYFlow-compatible nodes and edges for DAG visualization.
 */

import dagre from '@dagrejs/dagre';
import type { Node, Edge } from '@xyflow/svelte';
import type { AncestryNode, ProvenanceGraphNode, ProvenanceGraphEdge } from '$lib/types/provenance';

/** Node data stored in the XYFlow node. */
export type ProvenanceNodeData = ProvenanceGraphNode;

const NODE_WIDTH = 260;
const NODE_HEIGHT = 80;

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
	TokenCreated: '#10b981', // green
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
		// Seed token vs signal-injected
		const hasCrossNet = node.tokens.some((t) => t.role === 'produced');
		return hasCrossNet ? node.place_name ?? 'Token Created' : 'Process Root';
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
export function buildProvenanceGraph(ancestry: AncestryNode[]): {
	nodes: ProvenanceGraphNode[];
	edges: ProvenanceGraphEdge[];
} {
	// Group by event (net_id:event_seq)
	const eventMap = new Map<string, ProvenanceGraphNode>();

	for (const node of ancestry) {
		const id = `${node.net_id}:${node.event_seq}`;
		if (!eventMap.has(id)) {
			eventMap.set(id, {
				id,
				event_type: node.event_type,
				transition_name: node.transition_name,
				effect_handler: node.effect_handler,
				net_id: node.net_id,
				event_seq: node.event_seq,
				timestamp: node.timestamp,
				place_name: node.place_name,
				tokens: []
			});
		}
		eventMap.get(id)!.tokens.push({
			token_id: node.token_id,
			role: node.role,
			place_name: node.place_name
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
	g.setGraph({ rankdir: 'BT', nodesep: 40, ranksep: 60 });

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
		type: ge.cross_net ? 'smoothstep' : 'default',
		animated: ge.cross_net,
		style: ge.cross_net ? 'stroke-dasharray: 5 5; stroke: #f97316;' : undefined
	}));

	return { nodes, edges };
}
