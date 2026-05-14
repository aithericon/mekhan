// Scope walk for Decision / Loop guards (Phase 3 typed-ports).
//
// The backend's `service/src/compiler/compile.rs::compute_scopes` is the
// source of truth at publish time. This TS twin lets the editor offer
// autocomplete, scope chips, and a structured guard builder without an API
// round-trip per keystroke.
//
// We mirror the backend rules:
//   - Topologically walk the DAG (loop_back edges excluded).
//   - At each node, scope = union of every DAG predecessor's scope + its own
//     declared output port fields.
//   - Loop nodes additionally expose `<loop_id>.iteration : number` in their
//     own scope so `loop_condition` can reference it.

import type { components } from '$lib/api/schema';

type WorkflowGraph = components['schemas']['WorkflowGraph'];
type WorkflowNode = WorkflowGraph['nodes'][number];
type WorkflowEdge = WorkflowGraph['edges'][number];
type WorkflowNodeData = WorkflowNode['data'];
type Port = components['schemas']['Port'];
type FieldKind = components['schemas']['FieldKind'];

export type ScopeEntry = {
	nodeId: string;
	nodeLabel: string;
	field: string;
	kind: FieldKind;
	/** Qualified identifier for code insertion (`<nodeId>.<field>`). */
	qualified: string;
};

/**
 * Compute the set of in-scope identifiers at every node. Returns a map
 * keyed by node id; each entry is the deduped list of `<upstream>.<field>`
 * identifiers visible at that node.
 *
 * Cycles (only possible via `loop_back` edges, which we already drop) are
 * impossible; on a malformed graph we just return whatever we could resolve.
 */
export function computeScopes(graph: WorkflowGraph): Map<string, ScopeEntry[]> {
	const nodes = new Map<string, WorkflowNode>();
	for (const n of graph.nodes) nodes.set(n.id, n);

	// Build adjacency on the DAG (skip loop_back).
	const incoming = new Map<string, WorkflowEdge[]>();
	for (const n of graph.nodes) incoming.set(n.id, []);
	for (const e of graph.edges) {
		if (e.type === 'loop_back') continue;
		incoming.get(e.target)?.push(e);
	}

	// Topological order via Kahn's algorithm. Anything left over (cycles, or
	// detached subgraphs) falls into an arbitrary order after the sortable
	// nodes; their scope will simply not see contributions from un-sorted
	// upstreams, which matches "best-effort" semantics.
	const indeg = new Map<string, number>();
	for (const n of graph.nodes) indeg.set(n.id, 0);
	for (const e of graph.edges) {
		if (e.type === 'loop_back') continue;
		indeg.set(e.target, (indeg.get(e.target) ?? 0) + 1);
	}
	const order: string[] = [];
	const queue: string[] = [];
	for (const [id, d] of indeg) if (d === 0) queue.push(id);
	while (queue.length) {
		const id = queue.shift()!;
		order.push(id);
		for (const e of graph.edges) {
			if (e.type === 'loop_back') continue;
			if (e.source !== id) continue;
			const next = (indeg.get(e.target) ?? 0) - 1;
			indeg.set(e.target, next);
			if (next === 0) queue.push(e.target);
		}
	}
	// Append any unsorted nodes (shouldn't happen on a valid graph) so they
	// still get an entry.
	for (const n of graph.nodes) if (!order.includes(n.id)) order.push(n.id);

	const result = new Map<string, ScopeEntry[]>();
	const accumulated = new Map<string, Map<string, ScopeEntry>>();

	for (const id of order) {
		const scope = new Map<string, ScopeEntry>();

		for (const edge of incoming.get(id) ?? []) {
			const predScope = accumulated.get(edge.source);
			if (predScope) {
				for (const [k, v] of predScope) scope.set(k, v);
			}
			const pred = nodes.get(edge.source);
			if (pred) {
				for (const port of outputPorts(pred.data)) {
					for (const f of port.fields ?? []) {
						const qualified = `${pred.id}.${f.name}`;
						scope.set(qualified, {
							nodeId: pred.id,
							nodeLabel: nodeLabel(pred),
							field: f.name,
							kind: f.kind,
							qualified
						});
					}
				}
			}
		}

		// Loop's own iteration counter.
		const self = nodes.get(id);
		if (self && self.data.type === 'loop') {
			const qualified = `${id}.iteration`;
			scope.set(qualified, {
				nodeId: id,
				nodeLabel: nodeLabel(self),
				field: 'iteration',
				kind: 'number',
				qualified
			});
		}

		accumulated.set(id, scope);
		const list = Array.from(scope.values()).sort((a, b) => a.qualified.localeCompare(b.qualified));
		result.set(id, list);
	}

	return result;
}

/**
 * Output ports declared for a node. Mirrors the Rust accessor for Start +
 * AutomatedStep; everything else returns an empty list until Phase 4 fills
 * in derived ports for HumanTask / Decision / etc.
 */
function outputPorts(data: WorkflowNodeData): Port[] {
	switch (data.type) {
		case 'start':
			return data.initial ? [data.initial] : [];
		case 'automated_step':
			return data.output ? [data.output] : [];
		default:
			return [];
	}
}

function nodeLabel(node: WorkflowNode): string {
	const d = node.data as { label?: string };
	return d.label ?? node.id;
}

/**
 * Lightweight wrapper around the Rhai scope module: extract every
 * `<ident>.<field>` reference from a guard string. The implementation mirrors
 * `service/src/compiler/rhai_scope.rs::extract_qualified_refs` closely enough
 * for editor-side feedback; the backend remains authoritative at publish.
 */
export function extractQualifiedRefs(source: string): { node: string; field: string }[] {
	const cleaned = stripCommentsAndStrings(source);
	const locals = collectLocals(cleaned);
	const out: { node: string; field: string }[] = [];
	const seen = new Set<string>();

	// Match identifier at a position that is not preceded by `.` or another
	// identifier char, followed by `.` and another identifier.
	const re = /(?<![.A-Za-z0-9_])([A-Za-z_][A-Za-z0-9_]*)\s*\.\s*([A-Za-z_][A-Za-z0-9_]*)/g;
	for (const m of cleaned.matchAll(re)) {
		const node = m[1];
		const field = m[2];
		if (RHAI_KEYWORDS.has(node) || locals.has(node)) continue;
		const key = `${node}.${field}`;
		if (seen.has(key)) continue;
		seen.add(key);
		out.push({ node, field });
	}
	return out;
}

function stripCommentsAndStrings(src: string): string {
	// Replace string + comment content with spaces preserving offsets, so
	// downstream regexes see the same shape as `src` without false matches.
	let out = '';
	let i = 0;
	while (i < src.length) {
		const c = src[i];
		if (c === '/' && src[i + 1] === '/') {
			while (i < src.length && src[i] !== '\n') {
				out += ' ';
				i++;
			}
			continue;
		}
		if (c === '/' && src[i + 1] === '*') {
			out += '  ';
			i += 2;
			while (i + 1 < src.length && !(src[i] === '*' && src[i + 1] === '/')) {
				out += src[i] === '\n' ? '\n' : ' ';
				i++;
			}
			if (i + 1 < src.length) {
				out += '  ';
				i += 2;
			}
			continue;
		}
		if (c === '"' || c === "'") {
			const q = c;
			out += ' ';
			i++;
			while (i < src.length && src[i] !== q) {
				if (src[i] === '\\' && i + 1 < src.length) {
					out += '  ';
					i += 2;
					continue;
				}
				out += src[i] === '\n' ? '\n' : ' ';
				i++;
			}
			if (i < src.length) {
				out += ' ';
				i++;
			}
			continue;
		}
		out += c;
		i++;
	}
	return out;
}

function collectLocals(src: string): Set<string> {
	const locals = new Set<string>();
	for (const m of src.matchAll(/\b(?:let|for)\s+([A-Za-z_][A-Za-z0-9_]*)/g)) {
		locals.add(m[1]);
	}
	return locals;
}

const RHAI_KEYWORDS = new Set([
	'true',
	'false',
	'let',
	'const',
	'if',
	'else',
	'switch',
	'for',
	'in',
	'while',
	'loop',
	'do',
	'until',
	'break',
	'continue',
	'return',
	'fn',
	'is_shared',
	'this',
	'import',
	'export',
	'as',
	'global',
	'call',
	'curry',
	'type_of',
	'print',
	'debug',
	'eval',
	'throw',
	'try',
	'catch',
	'private',
	'public'
]);
