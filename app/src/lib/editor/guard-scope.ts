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
import { listResourceTypes, type ResourceTypeInfo } from '$lib/api/resources';

type WorkflowGraph = components['schemas']['WorkflowGraph'];
type FieldKind = components['schemas']['FieldKind'];
type GuardDiagnosticDto = components['schemas']['GuardDiagnosticDto'];

export type ScopeEntry = {
	nodeId: string;
	nodeLabel: string;
	field: string;
	kind: FieldKind;
	/** Producer-namespaced identifier for code insertion: `<slug>.<field>`
	 *  for borrowed parked-producer data (e.g. `review.invoice_amount`), or
	 *  `input.<path>` for genuinely control-token-resident leaves. */
	qualified: string;
};

/** Result of one `/api/analyze` round-trip. `graphOk: false` means the
 *  compiler refused to scope the graph (dangling edge, missing End, cycle),
 *  in which case `scopes` is empty and `diagnostics` carries the reasons.
 *  Picker UIs should grey themselves out and surface the diagnostic. */
export type ScopeAnalysis = {
	scopes: Map<string, ScopeEntry[]>;
	graphOk: boolean;
	diagnostics: GuardDiagnosticDto[];
	/** True when the analyzer call itself failed (network / 5xx). Distinct
	 *  from `graphOk: false`, which is a deliberate compiler verdict. */
	requestFailed: boolean;
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
 * Returns the scope map keyed by node id, plus the `graph_ok` flag and any
 * diagnostics — both surfaceable by the IDE so an empty picker explains
 * itself. Best-effort: on any failure (network, 5xx) returns
 * `{ scopes: empty, graphOk: false, requestFailed: true }` — the editor
 * degrades, never throws.
 */
export async function fetchNodeScopes(graph: WorkflowGraph): Promise<ScopeAnalysis> {
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
		return {
			scopes: out,
			graphOk: surface.graph_ok ?? false,
			diagnostics: surface.diagnostics ?? [],
			requestFailed: false
		};
	} catch {
		return { scopes: out, graphOk: false, diagnostics: [], requestFailed: true };
	}
}

/**
 * Lazy, module-cached fetch of `/api/resources/types`. The registry is
 * compile-time on the server (built from `inventory::submit!`) — values
 * never change at runtime, so one fetch per session is enough. The
 * promise is cached so multiple pickers opening at once share one
 * network round-trip. Failures cache a rejected promise for ~5s to
 * avoid hammering the server, then refetch on the next call.
 */
let resourceTypesCache: Promise<ResourceTypeInfo[]> | null = null;
let resourceTypesCachedAt = 0;
const RESOURCE_TYPES_ERROR_TTL_MS = 5_000;

export function loadResourceTypes(): Promise<ResourceTypeInfo[]> {
	if (resourceTypesCache) return resourceTypesCache;
	const promise = listResourceTypes();
	resourceTypesCache = promise;
	resourceTypesCachedAt = Date.now();
	promise.catch(() => {
		// Reject cached only if the entry is still ours and the TTL elapsed,
		// so a transient error doesn't pin a rejection forever.
		setTimeout(() => {
			if (resourceTypesCache === promise && Date.now() - resourceTypesCachedAt >= RESOURCE_TYPES_ERROR_TTL_MS) {
				resourceTypesCache = null;
			}
		}, RESOURCE_TYPES_ERROR_TTL_MS);
	});
	return promise;
}

/** Test/HMR helper — drops the cache so the next `loadResourceTypes` re-fetches. */
export function _clearResourceTypesCache(): void {
	resourceTypesCache = null;
}

/**
 * Project the workflow's `graph.resources: { alias -> type }` block plus
 * the type registry into `ScopeEntry[]` shaped for `RefPicker`'s resource
 * tab. Each alias contributes one entry per field (public + secret); the
 * synthetic `nodeId` is `resource:<alias>` so it never collides with a
 * real producer slug.
 *
 * Field `kind` is best-effort:
 *  - `password`, `token`, `key`, `secret` → `text` (the value is a
 *    string; the kernel rewraps it before any inline use).
 *  - `port` → `number`.
 *  - everything else → `text` (most resource configs are strings).
 *
 * Returns `[]` when the workflow declares no resources OR the registry
 * doesn't know the alias's type — the picker hides its Resources tab
 * rather than rendering a stub.
 */
export function buildResourceScope(
	resources: Record<string, string> | undefined,
	types: ResourceTypeInfo[]
): ScopeEntry[] {
	if (!resources) return [];
	const out: ScopeEntry[] = [];
	const byName = new Map(types.map((t) => [t.name, t]));
	// Match the BTreeMap alphabetisation we mirror in `YjsGraphBinding`.
	const aliases = Object.keys(resources).sort();
	for (const alias of aliases) {
		const typeName = resources[alias];
		const info = byName.get(typeName);
		if (!info) continue;
		const fields = [...(info.public_fields ?? []), ...(info.secret_fields ?? [])];
		for (const field of fields) {
			out.push({
				nodeId: `resource:${alias}`,
				nodeLabel: alias,
				field,
				kind: field === 'port' ? 'number' : 'text',
				qualified: `${alias}.${field}`
			});
		}
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
