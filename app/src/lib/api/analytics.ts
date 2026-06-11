/**
 * File-analytics client (Starfish-style capacity/breakdown/growth, docs/32).
 *
 * Thin wrappers over the `/data/analytics/*` endpoints: a generic group-by
 * breakdown (which doubles as the treemap level loader via `under`/`depth`),
 * the growth timeseries over `inventory_snapshots`, and the manual snapshot
 * trigger. Filters reuse the inventory query DSL (`filter[field][op]`).
 *
 * DTOs come from the generated OpenAPI schema (`components['schemas'][...]`).
 */
import { rawJson } from './client';
import type { components } from './schema';

/** Dimensions the breakdown endpoint can group by. */
export type BreakdownDimension =
	| 'server'
	| 'extension'
	| 'size_class'
	| 'age'
	| 'mtime_age'
	| 'owner'
	| 'directory';

export type BreakdownBucket = components['schemas']['BreakdownBucket'];
export type BreakdownResponse = components['schemas']['BreakdownResponse'];
export type SnapshotPoint = components['schemas']['SnapshotPoint'];
export type SnapshotResult = components['schemas']['SnapshotResult'];

export async function getAnalyticsBreakdown(params: {
	group_by: BreakdownDimension;
	/** Directory prefix to descend under (directory dimension lazy drill). */
	under?: string;
	/** Path components to group by below `under` (directory dimension, 1..=8). */
	depth?: number;
	limit?: number;
	file_server_id?: string;
	status?: string;
	/** Restrict to copies with a content identity (hashed → in the catalogue). */
	hashed?: boolean;
	search?: string;
}): Promise<BreakdownResponse> {
	const qs = new URLSearchParams();
	qs.set('group_by', params.group_by);
	if (params.under) qs.set('under', params.under);
	if (params.depth !== undefined) qs.set('depth', String(params.depth));
	if (params.limit !== undefined) qs.set('limit', String(params.limit));
	if (params.file_server_id) qs.set('filter[file_server_id][eq]', params.file_server_id);
	if (params.status) qs.set('filter[status][eq]', params.status);
	if (params.hashed) qs.set('filter[content_hash][is_not_null]', 'true');
	if (params.search) qs.set('search', params.search);
	return rawJson(`/data/analytics/breakdown?${qs.toString()}`);
}

export async function getAnalyticsTimeseries(params: {
	dim: string;
	key?: string;
	file_server_id?: string;
	bucket_secs?: number;
	window_secs?: number;
}): Promise<SnapshotPoint[]> {
	const qs = new URLSearchParams();
	qs.set('dim', params.dim);
	if (params.key) qs.set('key', params.key);
	if (params.file_server_id) qs.set('file_server_id', params.file_server_id);
	if (params.bucket_secs !== undefined) qs.set('bucket_secs', String(params.bucket_secs));
	if (params.window_secs !== undefined) qs.set('window_secs', String(params.window_secs));
	return rawJson(`/data/analytics/timeseries?${qs.toString()}`);
}

/** Manually capture a snapshot row-set (same writer as the background job). */
export async function triggerAnalyticsSnapshot(): Promise<SnapshotResult> {
	return rawJson('/data/analytics/snapshot', { method: 'POST' });
}
