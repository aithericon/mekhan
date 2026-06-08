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
export type DataEntriesResponse = components['schemas']['DataEntriesResponse'];

/** Paginated catalogued entries (+ copies) and an uncatalogued peek. */
export async function listDataEntries(params?: {
	category?: string;
	search?: string;
	sort?: string;
	page?: number;
	page_size?: number;
}): Promise<DataEntriesResponse> {
	const qs = new URLSearchParams();
	if (params?.category) qs.set('filter[category][eq]', params.category);
	if (params?.search) qs.set('search', params.search);
	if (params?.sort) qs.set('sort', params.sort);
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	const query = qs.toString();
	return rawJson(`/data/entries${query ? `?${query}` : ''}`);
}
