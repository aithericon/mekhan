/**
 * Unified Data-browser read-model client (docs/32 §4.1).
 *
 * One view over the catalogue (logical) + inventory (physical): each catalogued
 * entry with its physical copies (server names resolved), plus a peek at
 * uncatalogued (index-only) files. Filters reuse the catalogue query DSL.
 */
import { rawJson, ApiError } from './client';
import { authFetch } from '$lib/auth/fetch';
import type { components } from './schema';

export type DataCopy = components['schemas']['DataCopy'];
export type DataEntry = components['schemas']['DataEntry'];
export type UncataloguedFile = components['schemas']['UncataloguedFile'];
export type DataEntriesResponse = components['schemas']['DataEntriesResponse'];
export type UncataloguedResponse = components['schemas']['UncataloguedResponse'];
export type FacetBucket = components['schemas']['FacetBucket'];
export type FacetsResponse = components['schemas']['FacetsResponse'];
export type QueryFieldDesc = components['schemas']['QueryFieldDesc'];
export type QueryFieldsResponse = components['schemas']['QueryFieldsResponse'];
export type SavedQuery = components['schemas']['SavedQuery'];
export type SavedQueryCreate = components['schemas']['SavedQueryCreate'];
export type SavedQueryUpdate = components['schemas']['SavedQueryUpdate'];
export type CatalogueDataType = components['schemas']['CatalogueDataType'];
export type DataTypeColumn = components['schemas']['DataTypeColumn'];
export type DataTypePromote = components['schemas']['DataTypePromote'];
export type DataTypeUpdate = components['schemas']['DataTypeUpdate'];

/** One catalogue filter triple — compiled to `filter[FIELD][OP]=VALUE`. */
export type FilterTriple = { field: string; op: string; value: string };

function setFilters(qs: URLSearchParams, filters?: FilterTriple[]) {
	for (const f of filters ?? []) {
		qs.set(`filter[${f.field}][${f.op}]`, f.value);
	}
}

/** Paginated catalogued entries (+ copies) and an uncatalogued peek. */
export async function listDataEntries(params?: {
	category?: string;
	search?: string;
	source_net?: string;
	content_hash?: string;
	/**
	 * Raw catalogue query DSL (the same text the QueryBar shows). When present
	 * it is the CANONICAL filter scope: the server compiles it (a single
	 * server-side compiler, relative dates re-resolved per request) and it
	 * supersedes `filters` / `search` / `file_metadata`. Pagination + sort still
	 * apply. This replaces the old client-side `compileQuery` → bracket-notation
	 * path for the data browser.
	 */
	q?: string;
	/** Generic catalogue filter triples — `filter[FIELD][OP]=VALUE`. */
	filters?: FilterTriple[];
	/** Raw JSON object passed to the catalogue `file_metadata` JSONB filter. */
	file_metadata?: string;
	sort?: string;
	page?: number;
	page_size?: number;
}): Promise<DataEntriesResponse> {
	const qs = new URLSearchParams();
	if (params?.q && params.q.trim()) {
		// Canonical DSL scope — server-compiled. Skip the structured filter/search
		// params (the server would ignore them anyway when `q` is present).
		qs.set('q', params.q);
	} else {
		if (params?.category) qs.set('filter[category][eq]', params.category);
		if (params?.source_net) qs.set('filter[source_net][eq]', params.source_net);
		if (params?.content_hash) qs.set('filter[content_hash][eq]', params.content_hash);
		setFilters(qs, params?.filters);
		if (params?.search) qs.set('search', params.search);
		if (params?.file_metadata) qs.set('file_metadata', params.file_metadata);
	}
	if (params?.sort) qs.set('sort', params.sort);
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	const query = qs.toString();
	return rawJson(`/data/entries${query ? `?${query}` : ''}`);
}

/**
 * Uncatalogued (index-only) peek + total count for the active workspace.
 *
 * Split off `listDataEntries`: the server-side anti-join scans the whole
 * workspace inventory against the whole catalogue (seconds at corpus scale) and
 * is independent of the list's filter/sort/page. The browser loads it lazily so
 * the entries list stays fast and doesn't recompute this on every query change.
 */
export async function listUncatalogued(): Promise<UncataloguedResponse> {
	return rawJson('/data/uncatalogued');
}

// ── Facets ──────────────────────────────────────────────────────────────────

/** Group-by buckets (count + bytes) over the scoped catalogue. */
export async function getCatalogueFacets(params: {
	group_by: string;
	limit?: number;
	/** Raw catalogue query DSL — canonical filter scope (supersedes the
	 *  structured `search`/`filters`/`file_metadata` when present). */
	q?: string;
	search?: string;
	filters?: FilterTriple[];
	/** Raw JSON object passed to the catalogue `file_metadata` JSONB filter. */
	file_metadata?: string;
}): Promise<FacetsResponse> {
	const qs = new URLSearchParams();
	qs.set('group_by', params.group_by);
	if (params.limit !== undefined) qs.set('limit', String(params.limit));
	if (params.q && params.q.trim()) {
		qs.set('q', params.q);
	} else {
		setFilters(qs, params.filters);
		if (params.search) qs.set('search', params.search);
		if (params.file_metadata) qs.set('file_metadata', params.file_metadata);
	}
	return rawJson(`/catalogue/facets?${qs.toString()}`);
}

// ── Query-fields registry ───────────────────────────────────────────────────
// Static per server build — cache the promise module-level so the field
// picker / known-fields validation never re-fetch.
let queryFieldsCache: Promise<QueryFieldsResponse> | null = null;

export function getCatalogueQueryFields(): Promise<QueryFieldsResponse> {
	if (!queryFieldsCache) {
		queryFieldsCache = rawJson<QueryFieldsResponse>('/catalogue/query-fields').catch((e) => {
			queryFieldsCache = null; // don't cache failures
			throw e;
		});
	}
	return queryFieldsCache;
}

// ── Saved queries ───────────────────────────────────────────────────────────

export async function listSavedQueries(): Promise<SavedQuery[]> {
	return rawJson('/catalogue/saved-queries');
}

export async function createSavedQuery(body: SavedQueryCreate): Promise<SavedQuery> {
	return rawJson('/catalogue/saved-queries', { method: 'POST', body: JSON.stringify(body) });
}

export async function updateSavedQuery(id: string, body: SavedQueryUpdate): Promise<SavedQuery> {
	return rawJson(`/catalogue/saved-queries/${id}`, {
		method: 'PATCH',
		body: JSON.stringify(body)
	});
}

export async function deleteSavedQuery(id: string): Promise<void> {
	// 204 No Content — can't go through rawJson (it parses a JSON body).
	const res = await authFetch(`/api/v1/catalogue/saved-queries/${id}`, { method: 'DELETE' });
	if (!res.ok) {
		throw new ApiError(res.status, await res.text());
	}
}

// ── Registered data types (schema promotion) ───────────────────────────────

export async function listDataTypes(): Promise<CatalogueDataType[]> {
	return rawJson('/catalogue/data-types');
}

/** Promote a schema digest to a named data type (server derives the columns). */
export async function createDataType(body: DataTypePromote): Promise<CatalogueDataType> {
	return rawJson('/catalogue/data-types', { method: 'POST', body: JSON.stringify(body) });
}

export async function updateDataType(id: string, body: DataTypeUpdate): Promise<CatalogueDataType> {
	return rawJson(`/catalogue/data-types/${id}`, {
		method: 'PATCH',
		body: JSON.stringify(body)
	});
}

export async function deleteDataType(id: string): Promise<void> {
	// 204 No Content — can't go through rawJson (it parses a JSON body).
	const res = await authFetch(`/api/v1/catalogue/data-types/${id}`, { method: 'DELETE' });
	if (!res.ok) {
		throw new ApiError(res.status, await res.text());
	}
}

// ── Copies-by-hash lookup (self-sufficient ArtifactCard) ────────────────────
// Call-sites that render an ArtifactCard outside the Data browser (process
// artifacts tab, lineage, provenance sheet) don't have the entry's physical
// copies at hand; the card fetches them itself through the same read-model.
// Tiny module cache + in-flight dedup so a page of 20 cards over the same few
// hashes doesn't fan out duplicate requests.
const copiesCache = new Map<string, Promise<DataCopy[]>>();

export function copiesForHash(contentHash: string): Promise<DataCopy[]> {
	let p = copiesCache.get(contentHash);
	if (!p) {
		p = listDataEntries({ content_hash: contentHash, page_size: 1 })
			.then((r) => r.items[0]?.copies ?? [])
			.catch(() => {
				copiesCache.delete(contentHash); // don't cache failures
				return [];
			});
		copiesCache.set(contentHash, p);
	}
	return p;
}
