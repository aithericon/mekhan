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
	/** Raw JSON object passed to the catalogue `file_metadata` JSONB filter. */
	file_metadata?: string;
	sort?: string;
	page?: number;
	page_size?: number;
}): Promise<DataEntriesResponse> {
	const qs = new URLSearchParams();
	if (params?.category) qs.set('filter[category][eq]', params.category);
	if (params?.source_net) qs.set('filter[source_net][eq]', params.source_net);
	if (params?.search) qs.set('search', params.search);
	if (params?.file_metadata) qs.set('file_metadata', params.file_metadata);
	if (params?.sort) qs.set('sort', params.sort);
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	const query = qs.toString();
	return rawJson(`/data/entries${query ? `?${query}` : ''}`);
}
