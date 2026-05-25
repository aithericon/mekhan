// Variable-picker scope for template-test assertions.
//
// The runner's synthetic scope is `{ result, steps.<slug>.output, start }`.
// This helper enumerates every statically-derivable path the picker can show:
//
//   - End nodes with `resultMapping` → `result.value.<targetField>`
//     (multi-End workflows each get their own group)
//   - AutomatedStep nodes with declared `output.fields` → `steps.<slug>.output.<field>`
//   - Start nodes' `initial.fields` → `start.<id>.<field>`
//     (keyed by Start `node.id` to match the runner's start-token map shape)
//
// Both the path picker and the expected-value picker pull from this single
// list — there's no reason for "what's assertable" and "what's referenceable
// as RHS" to diverge.

import type { components } from '$lib/api/schema';
import type { ScopeEntry } from './guard-scope';

type WorkflowGraph = components['schemas']['WorkflowGraph'];
type WorkflowNode = components['schemas']['WorkflowNode'];
type PortField = components['schemas']['PortField'];

/** Build the full assertion picker scope from a workflow graph. */
export function buildAssertionScope(graph: WorkflowGraph): ScopeEntry[] {
	const out: ScopeEntry[] = [];

	// Count Ends and Starts so single-of-a-kind groups read clean ("Done"
	// instead of "End: Done"); multi-of-a-kind disambiguates with a prefix.
	const endCount = graph.nodes.filter(
		(n) => n.data && n.data.type === 'end'
	).length;
	const startCount = graph.nodes.filter(
		(n) => n.data && n.data.type === 'start'
	).length;

	for (const node of graph.nodes) {
		const data = node.data;
		if (!data) continue;

		if (data.type === 'end') {
			pushEndEntries(node, data, endCount > 1, out);
		} else if (data.type === 'automated_step') {
			pushStepEntries(node, data, out);
		} else if (data.type === 'start') {
			pushStartEntries(node, data, startCount > 1, out);
		}
	}

	return out;
}

function pushEndEntries(
	node: WorkflowNode,
	data: Extract<WorkflowNode['data'], { type: 'end' }>,
	multi: boolean,
	out: ScopeEntry[]
): void {
	const mappings = data.resultMapping ?? [];
	if (mappings.length === 0) return;
	const baseLabel = data.label?.trim() || 'End';
	const groupLabel = multi ? `End: ${baseLabel}` : baseLabel;
	for (const m of mappings) {
		out.push({
			nodeId: node.id,
			nodeLabel: groupLabel,
			field: m.targetField,
			kind: 'json',
			qualified: `result.value.${m.targetField}`
		});
	}
}

function pushStepEntries(
	node: WorkflowNode,
	data: Extract<WorkflowNode['data'], { type: 'automated_step' }>,
	out: ScopeEntry[]
): void {
	const fields = (data.output?.fields ?? []) as PortField[];
	if (fields.length === 0) return;
	const slug = node.slug && node.slug.trim() !== '' ? node.slug : node.id;
	const baseLabel = data.label?.trim() || slug;
	for (const f of fields) {
		out.push({
			nodeId: node.id,
			nodeLabel: baseLabel,
			field: f.name,
			kind: f.kind ?? 'json',
			qualified: `steps.${slug}.output.${f.name}`
		});
	}
}

function pushStartEntries(
	node: WorkflowNode,
	data: Extract<WorkflowNode['data'], { type: 'start' }>,
	multi: boolean,
	out: ScopeEntry[]
): void {
	const fields = (data.initial?.fields ?? []) as PortField[];
	if (fields.length === 0) return;
	const baseLabel = data.label?.trim() || 'Start';
	const groupLabel = multi ? `Start: ${baseLabel}` : baseLabel;
	for (const f of fields) {
		out.push({
			nodeId: node.id,
			nodeLabel: groupLabel,
			field: f.name,
			kind: f.kind ?? 'json',
			// Single-Start workflows hoist fields to `start.<field>` so the
			// path doesn't double up (avoids the awkward `start.start.amount`).
			// Multi-Start namespaces by block_id (== node.id) to keep
			// colliding field names disambiguated. Mirrors the asymmetry in
			// `runner::build_start_scope` exactly — picker and resolver
			// cannot drift.
			qualified: multi
				? `start.${node.id}.${f.name}`
				: `start.${f.name}`
		});
	}
}
