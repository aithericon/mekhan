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
import {
	listResourceTypes,
	listResources,
	type ResourceTypeInfo,
	type ResourceSummary
} from '$lib/api/resources';
import {
	listAssets,
	getAssetType,
	type AssetSummary,
	type PortField
} from '$lib/api/assets';

type WorkflowGraph = components['schemas']['WorkflowGraph'];
type FieldKind = components['schemas']['FieldKind'];
type GuardDiagnosticDto = components['schemas']['GuardDiagnosticDto'];
export type TyDescriptor = components['schemas']['TyDescriptor'];

export type ScopeEntry = {
	nodeId: string;
	nodeLabel: string;
	field: string;
	kind: FieldKind;
	/** Producer-namespaced identifier for code insertion: `<slug>.<field>`
	 *  for borrowed parked-producer data (e.g. `review.invoice_amount`), or
	 *  `input.<path>` for genuinely control-token-resident leaves. */
	qualified: string;
	/** Full recursive type tree from the backend analyzer. Populated for
	 *  scope entries that came from the compiler; absent for resource
	 *  entries (built client-side from the resource type registry).
	 *  Pickers drill into `ty.fields` (Object) or `ty.element` (Array) to
	 *  surface nested + per-element refs without further round-trips. */
	ty?: TyDescriptor;
	/** For named-global entries (workspace resources / template assets), which
	 *  kind this is — the RefPicker splits the two into separate "Resources" and
	 *  "Assets" tabs. Absent for regular in-scope producer refs and for the
	 *  client-side resource fallback (treated as `'resource'`). */
	globalKind?: 'resource' | 'asset';
};

/** Result of one `/api/v1/analyze` round-trip. `graphOk: false` means the
 *  compiler refused to scope the graph (dangling edge, missing End, cycle),
 *  in which case `scopes` is empty and `diagnostics` carries the reasons.
 *  Picker UIs should grey themselves out and surface the diagnostic. */
export type ScopeAnalysis = {
	scopes: Map<string, ScopeEntry[]>;
	/** Server-authoritative "Globals" scope — workspace resources + template
	 *  assets resolved by the backend when `workspace_id`/`template_id` are
	 *  present in the analyze request. Entries are deduplicated (the server
	 *  emits the same set for every node; we keep one copy here). When the
	 *  server has no DB context (ids absent or scope empty), this is `[]`
	 *  and callers may fall back to client-side `buildResourceScope`. */
	globalsScope: ScopeEntry[];
	graphOk: boolean;
	diagnostics: GuardDiagnosticDto[];
	/** True when the analyzer call itself failed (network / 5xx). Distinct
	 *  from `graphOk: false`, which is a deliberate compiler verdict. */
	requestFailed: boolean;
};

/** Reduce a [`TyDescriptor`] to the legacy single-label `FieldKind` for
 *  callers that haven't moved to the recursive tree. The label matches the
 *  string the backend used to emit on `ScopeEntryDto.ty`, so behavior is
 *  byte-identical to before. */
export function tyDescriptorToFieldKind(ty: TyDescriptor | undefined): FieldKind {
	if (!ty) return 'json';
	switch (ty.kind) {
		case 'scalar':
			switch (ty.name) {
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
		default:
			return 'json';
	}
}

/** Human-readable label for a [`TyDescriptor`], used by picker badges.
 *  Arrays render as `array<T>` (recursing on `element`); objects render as
 *  `{a, b, c}`; scalars use their raw name; `any` / `opaque` use their
 *  canonical labels. */
export function tyDescriptorLabel(ty: TyDescriptor | undefined): string {
	if (!ty) return 'unknown';
	switch (ty.kind) {
		case 'scalar':
			return ty.name;
		case 'array':
			return `array<${tyDescriptorLabel(ty.element)}>`;
		case 'object': {
			const keys = Object.keys(ty.fields);
			if (keys.length === 0) return 'object';
			if (keys.length <= 3) return `{${keys.join(', ')}}`;
			return `{${keys.slice(0, 3).join(', ')}, …}`;
		}
		case 'any':
			return 'any';
		case 'opaque':
			return `Opaque(${ty.name})`;
	}
}

/** Template / workspace context forwarded to the analyze endpoint so the
 *  backend can resolve workspace-scoped resources and template-visible assets
 *  into the "Globals" scope group. Both are optional: the analyze call still
 *  works without them — the Globals group will simply be empty. */
export type AnalyzeContext = {
	templateId?: string | null;
	workspaceId?: string | null;
};

/**
 * Fetch the in-scope identifiers at every node from the backend analyzer.
 * Returns the scope map keyed by node id, plus the `graph_ok` flag and any
 * diagnostics — both surfaceable by the IDE so an empty picker explains
 * itself. Best-effort: on any failure (network, 5xx) returns
 * `{ scopes: empty, graphOk: false, requestFailed: true }` — the editor
 * degrades, never throws.
 *
 * When `ctx` carries `templateId` / `workspaceId`, the backend resolves
 * named globals (workspace resources + template assets) and returns them as
 * entries with `producer_label === "Globals"`. This function separates those
 * entries into `ScopeAnalysis.globalsScope` (deduplicated — the server emits
 * the same set for every node) and strips them from the per-node `scopes`
 * map so the RefPicker's "Globals" tab and "Refs" tab are disjoint.
 */
export async function fetchNodeScopes(
	graph: WorkflowGraph,
	ctx?: AnalyzeContext
): Promise<ScopeAnalysis> {
	const out = new Map<string, ScopeEntry[]>();
	try {
		const surface = await analyzeGraph({
			graph,
			name: 'editor',
			description: '',
			files: {},
			...(ctx?.templateId ? { template_id: ctx.templateId } : {}),
			...(ctx?.workspaceId ? { workspace_id: ctx.workspaceId } : {})
		});

		// Named-global entries are synthetic (empty `producer_node`) and labelled
		// by kind — `producer_label === "Resource"` or `"Asset"` — with the
		// global's name carried in `note` (so the picker groups by name). The
		// server emits the same global set for every node; we deduplicate by
		// `path` and keep one canonical copy in `globalsScope`, tagged with
		// `globalKind` so the RefPicker can split Resources vs Assets into
		// separate tabs.
		const seenGlobalPaths = new Set<string>();
		const globalsScope: ScopeEntry[] = [];

		for (const [nodeId, entries] of Object.entries(surface.scopes ?? {})) {
			const regularEntries: ScopeEntry[] = [];
			for (const e of entries ?? []) {
				const isGlobal =
					e.producer_node === '' &&
					(e.producer_label === 'Resource' || e.producer_label === 'Asset');
				const mapped: ScopeEntry = {
					nodeId: e.producer_node,
					// Group globals by their own name (carried in `note`); regular
					// entries keep their producer's label.
					nodeLabel: isGlobal ? e.note || e.producer_label : e.producer_label,
					field: e.path.split('.').pop() ?? e.path,
					kind: tyDescriptorToFieldKind(e.ty),
					qualified: e.path,
					ty: e.ty,
					...(isGlobal
						? { globalKind: e.producer_label === 'Asset' ? 'asset' : 'resource' }
						: {})
				};
				if (isGlobal) {
					// Deduplicate across nodes (all nodes share the same global set).
					if (!seenGlobalPaths.has(e.path)) {
						seenGlobalPaths.add(e.path);
						globalsScope.push(mapped);
					}
				} else {
					regularEntries.push(mapped);
				}
			}
			out.set(nodeId, regularEntries);
		}

		return {
			scopes: out,
			globalsScope,
			graphOk: surface.graph_ok ?? false,
			diagnostics: surface.diagnostics ?? [],
			requestFailed: false
		};
	} catch {
		return { scopes: out, globalsScope: [], graphOk: false, diagnostics: [], requestFailed: true };
	}
}

/**
 * Lazy, module-cached fetch of `/api/v1/resources/types`. The registry is
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
		setTimeout(() => {
			if (resourceTypesCache === promise && Date.now() - resourceTypesCachedAt >= RESOURCE_TYPES_ERROR_TTL_MS) {
				resourceTypesCache = null;
			}
		}, RESOURCE_TYPES_ERROR_TTL_MS);
	});
	return promise;
}

/**
 * Workspace resources — like the type registry, fetched once per session
 * and shared across all pickers. Cached promise + 5s error TTL mirror the
 * type-registry pattern. Unlike types, resources DO change at runtime
 * (CRUD from `/resources`), but the editor's "Refresh" affordance + a
 * full page reload pick up changes — keeping the picker reactive to
 * every mutation would require a websocket the resources kernel doesn't
 * carry yet.
 */
let workspaceResourcesCache: Promise<ResourceSummary[]> | null = null;
let workspaceResourcesCachedAt = 0;

export function loadWorkspaceResources(): Promise<ResourceSummary[]> {
	if (workspaceResourcesCache) return workspaceResourcesCache;
	const promise = listResources({ perPage: 200 }).then((page) => page.items);
	workspaceResourcesCache = promise;
	workspaceResourcesCachedAt = Date.now();
	promise.catch(() => {
		setTimeout(() => {
			if (workspaceResourcesCache === promise && Date.now() - workspaceResourcesCachedAt >= RESOURCE_TYPES_ERROR_TTL_MS) {
				workspaceResourcesCache = null;
			}
		}, RESOURCE_TYPES_ERROR_TTL_MS);
	});
	return promise;
}

/** Test/HMR helper — drops the caches so the next loaders re-fetch. */
export function _clearResourceTypesCache(): void {
	resourceTypesCache = null;
	workspaceResourcesCache = null;
}

/**
 * Project the workspace's resources + the type registry into
 * `ScopeEntry[]` shaped for `RefPicker`'s Resources tab. Each workspace
 * resource contributes one entry per field of its declared type (public
 * + secret); the synthetic `nodeId` is `resource:<name>` so it never
 * collides with a real producer slug.
 *
 * The compiler resolves `<name>.<field>` directly against the workspace's
 * resource list at publish time (no per-workflow alias indirection), so
 * the picker shows the same set of resources for every workflow — they
 * are workspace-scoped, not workflow-scoped.
 *
 * Field `kind` is best-effort:
 *  - `port` → `number`
 *  - everything else → `text` (most resource configs are strings)
 *
 * Resources whose type is not in the registry are dropped silently —
 * the picker only surfaces what the user can actually consume.
 */
export function buildResourceScope(
	resources: ResourceSummary[] | undefined,
	types: ResourceTypeInfo[]
): ScopeEntry[] {
	if (!resources || resources.length === 0) return [];
	const out: ScopeEntry[] = [];
	const byType = new Map(types.map((t) => [t.name, t]));
	// `path` is the workspace-unique key the compiler matches against
	// Python `<head>.<field>` source patterns — alphabetise by it so the
	// picker order matches what the user types.
	const sorted = [...resources].sort((a, b) => a.path.localeCompare(b.path));
	for (const resource of sorted) {
		const info = byType.get(resource.resource_type);
		if (!info) continue;
		// Dynamic-fields (kv) resources surface the user-supplied key list
		// in `dynamic_keys`. Typed resources use the descriptor's static
		// `public_fields ∪ secret_fields`. Either way the picker emits
		// `<path>.<field>` entries that match what the compiler resolves.
		const fields = resource.dynamic_keys ?? [
			...(info.public_fields ?? []),
			...(info.secret_fields ?? [])
		];
		for (const field of fields) {
			out.push({
				nodeId: `resource:${resource.id}`,
				// Use display_name when present (more human-readable in the
				// picker's left column); fall back to path so an unnamed
				// resource still has a label. Pickers showing the full
				// `<path>.<field>` qualified form keep the contract clear.
				nodeLabel: resource.display_name || resource.path,
				field,
				kind: field === 'port' ? 'number' : 'text',
				qualified: `${resource.path}.${field}`,
				globalKind: 'resource'
			});
		}
	}
	return out;
}

/**
 * Project template-visible assets + their type fields into `ScopeEntry[]`
 * for `RefPicker`'s Assets tab — the asset analogue of `buildResourceScope`.
 *
 * Each asset contributes one entry per field of its declared type, keyed by
 * the asset's `ref_key` (the identifier the compiler matches `<ref_key>.<field>`
 * against). `typesById` maps an asset's `type_id` to its ordered `PortField`
 * list (asset type summaries don't carry fields — the caller fetches each
 * type's detail; see `loadTemplateAssetScope`). Assets whose type is missing
 * from the map are dropped silently.
 *
 * Unlike resources (workspace-scoped), assets are template-visible: the set
 * shown depends on the template's scope chain, resolved server-side by the
 * `/api/v1/assets?scope=template:<id>` query the loader issues.
 */
export function buildAssetScope(
	assets: AssetSummary[] | undefined,
	typesById: Map<string, PortField[]>
): ScopeEntry[] {
	if (!assets || assets.length === 0) return [];
	const out: ScopeEntry[] = [];
	// Alphabetise by `ref_key` so the picker order matches what the user types.
	const sorted = [...assets].sort((a, b) => a.ref_key.localeCompare(b.ref_key));
	for (const asset of sorted) {
		const fields = typesById.get(asset.type_id);
		if (!fields) continue;
		for (const field of fields) {
			out.push({
				nodeId: `asset:${asset.id}`,
				nodeLabel: asset.display_name || asset.ref_key,
				field: field.name,
				kind: field.kind,
				qualified: `${asset.ref_key}.${field.name}`,
				globalKind: 'asset'
			});
		}
	}
	return out;
}

/**
 * Template-asset library for the RefPicker's Assets tab. Fetches every
 * template-visible asset plus the field list of each distinct asset type, then
 * projects them via `buildAssetScope`. Cached per-template (mirrors the
 * resource caches): assets DO change at runtime, but the editor's "Refresh"
 * affordance + a full page reload pick up changes.
 *
 * Resolves to `[]` (never rejects) on any failure so the picker degrades to
 * its other tabs instead of throwing.
 */
const templateAssetScopeCache = new Map<string, Promise<ScopeEntry[]>>();

export function loadTemplateAssetScope(templateId: string): Promise<ScopeEntry[]> {
	const cached = templateAssetScopeCache.get(templateId);
	if (cached) return cached;
	const promise = (async () => {
		const page = await listAssets({
			scope: { kind: 'template', id: templateId },
			perPage: 200
		});
		const assets = page.items;
		const typeIds = [...new Set(assets.map((a) => a.type_id))];
		const typesById = new Map<string, PortField[]>();
		await Promise.all(
			typeIds.map(async (id) => {
				try {
					const detail = await getAssetType(id);
					typesById.set(id, detail.fields);
				} catch {
					// Skip a type we can't resolve — its assets just won't list.
				}
			})
		);
		return buildAssetScope(assets, typesById);
	})();
	templateAssetScopeCache.set(templateId, promise);
	promise.catch(() => {
		// Drop the rejected promise so the next call retries.
		if (templateAssetScopeCache.get(templateId) === promise) {
			templateAssetScopeCache.delete(templateId);
		}
	});
	return promise;
}

/** Test/HMR helper — drops the template-asset scope cache. */
export function _clearAssetScopeCache(): void {
	templateAssetScopeCache.clear();
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
