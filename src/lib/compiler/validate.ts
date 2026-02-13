import type { WorkflowGraph, WorkflowNodeData } from '$lib/types/editor';

export type ValidationError = {
	nodeId?: string;
	message: string;
};

/** Validate a workflow graph before compilation */
export function validateGraph(graph: WorkflowGraph): ValidationError[] {
	const errors: ValidationError[] = [];

	// Must have nodes
	if (graph.nodes.length === 0) {
		errors.push({ message: 'Workflow has no nodes' });
		return errors;
	}

	// Exactly one Start node
	const startNodes = graph.nodes.filter((n) => n.data.type === 'start');
	if (startNodes.length === 0) {
		errors.push({ message: 'Workflow must have a Start node' });
	} else if (startNodes.length > 1) {
		for (const n of startNodes.slice(1)) {
			errors.push({ nodeId: n.id, message: 'Only one Start node is allowed' });
		}
	}

	// At least one End node
	const endNodes = graph.nodes.filter((n) => n.data.type === 'end');
	if (endNodes.length === 0) {
		errors.push({ message: 'Workflow must have at least one End node' });
	}

	// Build adjacency for reachability
	const nodeIds = new Set(graph.nodes.map((n) => n.id));
	const outgoing = new Map<string, string[]>();
	const incoming = new Map<string, string[]>();

	for (const edge of graph.edges) {
		if (!nodeIds.has(edge.source) || !nodeIds.has(edge.target)) continue;
		if (!outgoing.has(edge.source)) outgoing.set(edge.source, []);
		outgoing.get(edge.source)!.push(edge.target);
		if (!incoming.has(edge.target)) incoming.set(edge.target, []);
		incoming.get(edge.target)!.push(edge.source);
	}

	// Check all nodes reachable from Start
	if (startNodes.length === 1) {
		const reachable = new Set<string>();
		const queue = [startNodes[0].id];
		while (queue.length > 0) {
			const current = queue.shift()!;
			if (reachable.has(current)) continue;
			reachable.add(current);
			for (const next of outgoing.get(current) ?? []) {
				if (!reachable.has(next)) queue.push(next);
			}
		}

		for (const node of graph.nodes) {
			if (!reachable.has(node.id) && node.data.type !== 'start') {
				errors.push({
					nodeId: node.id,
					message: `Node "${node.data.label}" is not reachable from Start`
				});
			}
		}
	}

	// Check Start has no incoming edges
	for (const sn of startNodes) {
		if ((incoming.get(sn.id) ?? []).length > 0) {
			errors.push({ nodeId: sn.id, message: 'Start node must not have incoming connections' });
		}
	}

	// Check End has no outgoing edges
	for (const en of endNodes) {
		if ((outgoing.get(en.id) ?? []).length > 0) {
			errors.push({ nodeId: en.id, message: 'End node must not have outgoing connections' });
		}
	}

	// Check non-terminal nodes have at least one outgoing edge
	for (const node of graph.nodes) {
		if (node.data.type === 'end') continue;
		if ((outgoing.get(node.id) ?? []).length === 0) {
			errors.push({
				nodeId: node.id,
				message: `Node "${node.data.label}" has no outgoing connections`
			});
		}
	}

	// Check non-start nodes have at least one incoming edge
	for (const node of graph.nodes) {
		if (node.data.type === 'start') continue;
		if ((incoming.get(node.id) ?? []).length === 0) {
			errors.push({
				nodeId: node.id,
				message: `Node "${node.data.label}" has no incoming connections`
			});
		}
	}

	// Validate human task nodes have at least one step
	for (const node of graph.nodes) {
		if (node.data.type === 'human_task') {
			if (!node.data.steps || node.data.steps.length === 0) {
				errors.push({
					nodeId: node.id,
					message: `Human Task "${node.data.label}" has no steps configured`
				});
			}
		}
	}

	// Check for unique field names across all human tasks
	const fieldNames = new Map<string, string>(); // name -> first node label
	for (const node of graph.nodes) {
		if (node.data.type !== 'human_task') continue;
		for (const step of node.data.steps ?? []) {
			for (const block of step.blocks) {
				if (block.type === 'input') {
					const existing = fieldNames.get(block.field.name);
					if (existing && existing !== node.data.label) {
						errors.push({
							nodeId: node.id,
							message: `Field name "${block.field.name}" is used in both "${existing}" and "${node.data.label}". Field names must be unique across the workflow.`
						});
					}
					fieldNames.set(block.field.name, node.data.label);
				}
			}
		}
	}

	return errors;
}
