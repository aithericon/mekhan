// Variable-picker scope for template-test assertions.
//
// The runner's synthetic assertion scope is `{ result, steps.<slug>.output }`.
// This helper enumerates the statically-derivable `result.value.<field>`
// paths from each End node's declared `resultMapping`. Each End is its own
// group so a multi-End workflow surfaces every possible result shape side by
// side — the author picks against the branch their test actually reaches.
//
// Step-output paths (`steps.<slug>.output.<field>`) are intentionally NOT
// included here: the Available-scope card already renders the literal
// reference_scope snapshot for those, and the user explicitly asked the
// picker to be restricted to response nodes' result.

import type { components } from '$lib/api/schema';
import type { ScopeEntry } from './guard-scope';

type WorkflowGraph = components['schemas']['WorkflowGraph'];

/** Build the assertion picker's `ScopeEntry[]` from a workflow graph.
 *  Returns an empty array when no End node declares a `resultMapping`. */
export function buildAssertionScope(graph: WorkflowGraph): ScopeEntry[] {
	const out: ScopeEntry[] = [];

	// Count Ends so we can disambiguate group labels only when needed. A
	// single-End workflow uses the End's own label; multi-End prepends
	// "End: " so the picker reads as `End: Done` / `End: Rejected`.
	const endCount = graph.nodes.filter(
		(n) => n.data && n.data.type === 'end'
	).length;

	for (const node of graph.nodes) {
		const data = node.data;
		if (!data || data.type !== 'end') continue;

		const mappings = data.resultMapping ?? [];
		if (mappings.length === 0) continue;

		const baseLabel = data.label?.trim() || 'End';
		const groupLabel = endCount > 1 ? `End: ${baseLabel}` : baseLabel;

		for (const m of mappings) {
			out.push({
				nodeId: node.id,
				nodeLabel: groupLabel,
				field: m.targetField,
				// `resultMapping` entries are Rhai expressions producing arbitrary
				// shapes — the editor has no static type for them. `json` is the
				// RefPicker's "addressable but untyped" leaf.
				kind: 'json',
				qualified: `result.value.${m.targetField}`
			});
		}
	}

	return out;
}
