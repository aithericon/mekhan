// Guard scope for Decision / Loop editors.
//
// SINGLE SOURCE OF TRUTH: the backend shape-aware analyzer
// (`service/src/compiler/token_shape.rs`, exposed at `POST /api/analyze`) is
// now the *only* implementation of scope rules. The editor no longer mirrors
// the lowering — it asks the compiler. This kills the duplicate-scope class
// of bug: what the picker shows is exactly what the compiler resolves.
//
// `extractQualifiedRefs` stays here: it's a pure Rhai-text utility (find
// `<ident>.<field>` references for highlighting), not a reimplementation of
// scope rules.

import type { components } from '$lib/api/schema';
import { analyzeGraph } from '$lib/api/client';

type WorkflowGraph = components['schemas']['WorkflowGraph'];
type FieldKind = components['schemas']['FieldKind'];

export type ScopeEntry = {
	nodeId: string;
	nodeLabel: string;
	field: string;
	kind: FieldKind;
	/** Qualified identifier for code insertion (the real shape path,
	 *  e.g. `input.data.invoice_amount`). */
	qualified: string;
};

/** Backend type label → editor `FieldKind`. Non-scalar shapes (Object,
 *  Array, Any, Opaque) collapse to `json` — addressable but untyped at the
 *  leaf the picker offers. */
function tyToFieldKind(ty: string): FieldKind {
	switch (ty) {
		case 'String':
			return 'text';
		case 'Number':
			return 'number';
		case 'Bool':
			return 'bool';
		case 'FileRef':
			return 'file';
		case 'Timestamp':
			return 'timestamp';
		default:
			return 'json';
	}
}

/**
 * Fetch the in-scope identifiers at every node from the backend analyzer.
 * Returns a map keyed by node id. Best-effort: on any failure (network, or a
 * draft too broken to analyze) returns an empty map, matching the previous
 * "show whatever resolved" behavior — the editor degrades, never throws.
 */
export async function fetchNodeScopes(
	graph: WorkflowGraph
): Promise<Map<string, ScopeEntry[]>> {
	const out = new Map<string, ScopeEntry[]>();
	try {
		const surface = await analyzeGraph({
			graph,
			name: 'editor',
			description: '',
			files: {}
		});
		for (const [nodeId, entries] of Object.entries(surface.scopes ?? {})) {
			out.set(
				nodeId,
				(entries ?? []).map((e) => ({
					nodeId: e.producer_node,
					nodeLabel: e.producer_label,
					field: e.path.split('.').pop() ?? e.path,
					kind: tyToFieldKind(e.ty),
					qualified: e.path
				}))
			);
		}
	} catch {
		// best-effort: editor still works without scope chips
	}
	return out;
}

/**
 * Extract every `<ident>.<field>` reference from a guard string. Pure
 * editor-side Rhai text analysis (for highlighting / unknown-ref hints);
 * the backend remains authoritative for resolution.
 */
export function extractQualifiedRefs(source: string): { node: string; field: string }[] {
	const cleaned = stripCommentsAndStrings(source);
	const locals = collectLocals(cleaned);
	const out: { node: string; field: string }[] = [];
	const seen = new Set<string>();

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
