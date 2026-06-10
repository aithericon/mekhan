/**
 * Unified Data-browser read-model client (docs/32 §4.1).
 *
 * One view over the catalogue (logical) + inventory (physical): each catalogued
 * entry with its physical copies (server names resolved), plus a peek at
 * uncatalogued (index-only) files. Filters reuse the catalogue query DSL.
 */
import { rawJson } from './client';
import type { components } from './schema';

export type DataCopy = components['schemas']['DataCopy'];
export type DataEntry = components['schemas']['DataEntry'];
export type UncataloguedFile = components['schemas']['UncataloguedFile'];
export type DataEntriesResponse = components['schemas']['DataEntriesResponse'];

/** Paginated catalogued entries (+ copies) and an uncatalogued peek. */
export async function listDataEntries(params?: {
	category?: string;
	search?: string;
	source_net?: string;
	content_hash?: string;
	/** Raw JSON object passed to the catalogue `file_metadata` JSONB filter. */
	file_metadata?: string;
	sort?: string;
	page?: number;
	page_size?: number;
}): Promise<DataEntriesResponse> {
	const qs = new URLSearchParams();
	if (params?.category) qs.set('filter[category][eq]', params.category);
	if (params?.source_net) qs.set('filter[source_net][eq]', params.source_net);
	if (params?.content_hash) qs.set('filter[content_hash][eq]', params.content_hash);
	if (params?.search) qs.set('search', params.search);
	if (params?.file_metadata) qs.set('file_metadata', params.file_metadata);
	if (params?.sort) qs.set('sort', params.sort);
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	const query = qs.toString();
	return rawJson(`/data/entries${query ? `?${query}` : ''}`);
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
