/**
 * Typed wrappers for the docs/20 Asset + Asset-Type CRUD endpoints.
 *
 * Mirrors `$lib/api/resources.ts`: a fresh `openapi-fetch` client instance
 * (rather than extending the 1300-line `client.ts` monolith — keeps merge
 * friction low), the older "throws on non-2xx" contract, and re-exported
 * `components['schemas'][...]` aliases so call sites don't reach into
 * `schema.d.ts` directly.
 *
 * The multipart endpoints (file upload + CSV import) can't go through
 * openapi-fetch cleanly, so they use `authFetch` + `FormData` directly
 * (mirrors `client.ts`'s `uploadFile`). Errors there throw `ApiError` from
 * `client.ts` so structured `code` handling (incomparable-scope clash,
 * additive-only rejection) is available to callers.
 */
import createClient, { type Middleware } from 'openapi-fetch';
import type { components, paths } from './schema';
import { ApiError } from './client';
import { authFetch } from '$lib/auth/fetch';

const API_BASE = '/api/v1';

const sessionExpiryMiddleware: Middleware = {
	async onResponse({ response, request }) {
		if (
			response.status === 401 &&
			typeof window !== 'undefined' &&
			!new URL(request.url).pathname.startsWith('/api/auth/')
		) {
			const here = window.location.pathname + window.location.search;
			window.location.assign(`/api/auth/login?return_to=${encodeURIComponent(here)}`);
		}
		return response;
	}
};

const client = createClient<paths>({ baseUrl: '', credentials: 'same-origin' });
client.use(sessionExpiryMiddleware);

// ── Type aliases ───────────────────────────────────────────────────────────

export type AssetTypeSummary = components['schemas']['AssetTypeSummary'];
export type AssetTypeDetail = components['schemas']['AssetTypeDetail'];
export type CreateAssetTypeRequest = components['schemas']['CreateAssetTypeRequest'];
export type UpdateAssetTypeRequest = components['schemas']['UpdateAssetTypeRequest'];
export type AssetSummary = components['schemas']['AssetSummary'];
export type AssetDetail = components['schemas']['AssetDetail'];
export type CreateAssetRequest = components['schemas']['CreateAssetRequest'];
export type ReplaceRecordsRequest = components['schemas']['ReplaceRecordsRequest'];
export type AssetFileUploadResponse = components['schemas']['AssetFileUploadResponse'];
export type AssetBinding = components['schemas']['AssetBinding'];
export type PortField = components['schemas']['PortField'];
export type FieldKind = components['schemas']['FieldKind'];
export type Cardinality = components['schemas']['Cardinality'];
export type ScopeKind = components['schemas']['ScopeKind'];
export type PaginatedAssetTypes = components['schemas']['PaginatedResponse_AssetTypeSummary'];
export type PaginatedAssets = components['schemas']['PaginatedResponse_AssetSummary'];
export type AssetUsageItem = components['schemas']['AssetUsageItem'];
export type PaginatedAssetUsage = components['schemas']['PaginatedResponse_AssetUsageItem'];

/** A single validated asset record row (the JSONB shape is opaque to the client). */
export type AssetRecord = Record<string, unknown>;

// ── Helpers ────────────────────────────────────────────────────────────────

function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
	if (result.error !== undefined) {
		throw new ApiError(
			result.response.status,
			result.error as Record<string, unknown> | string | undefined
		);
	}
	if (result.data === undefined) {
		throw new ApiError(result.response.status, 'empty body');
	}
	return result.data;
}

/**
 * Scope context for downward-visibility resolution. Format on the wire:
 * `workspace`, `folder:<uuid>`, or `template:<uuid>`. Omit for the caller's
 * workspace.
 */
export type ScopeContext =
	| { kind: 'workspace' }
	| { kind: 'folder'; id: string }
	| { kind: 'template'; id: string };

export function scopeToParam(scope?: ScopeContext): string | undefined {
	if (!scope) return undefined;
	if (scope.kind === 'workspace') return 'workspace';
	return `${scope.kind}:${scope.id}`;
}

// ── Asset-type endpoints ─────────────────────────────────────────────────────

export interface ListAssetTypesParams {
	page?: number;
	perPage?: number;
	scope?: ScopeContext;
	folder?: string;
}

export async function listAssetTypes(
	params?: ListAssetTypesParams
): Promise<PaginatedAssetTypes> {
	return unwrap(
		await client.GET('/api/v1/asset-types', {
			params: {
				query: {
					page: params?.page ?? 0,
					per_page: params?.perPage ?? 200,
					scope: scopeToParam(params?.scope),
					folder: params?.folder
				}
			}
		})
	);
}

export async function createAssetType(
	body: CreateAssetTypeRequest
): Promise<AssetTypeDetail> {
	return unwrap(await client.POST('/api/v1/asset-types', { body }));
}

export async function getAssetType(id: string): Promise<AssetTypeDetail> {
	return unwrap(await client.GET('/api/v1/asset-types/{id}', { params: { path: { id } } }));
}

export async function updateAssetType(
	id: string,
	body: UpdateAssetTypeRequest
): Promise<AssetTypeDetail> {
	return unwrap(
		await client.PUT('/api/v1/asset-types/{id}', { params: { path: { id } }, body })
	);
}

export async function deleteAssetType(id: string): Promise<void> {
	const res = await client.DELETE('/api/v1/asset-types/{id}', { params: { path: { id } } });
	if (res.response.status >= 400) {
		throw new ApiError(
			res.response.status,
			res.error as Record<string, unknown> | string | undefined
		);
	}
}

// ── Asset endpoints ──────────────────────────────────────────────────────────

export interface ListAssetsParams {
	page?: number;
	perPage?: number;
	type_id?: string;
	scope?: ScopeContext;
	folder?: string;
}

export async function listAssets(params?: ListAssetsParams): Promise<PaginatedAssets> {
	return unwrap(
		await client.GET('/api/v1/assets', {
			params: {
				query: {
					page: params?.page ?? 0,
					per_page: params?.perPage ?? 200,
					type_id: params?.type_id,
					scope: scopeToParam(params?.scope),
					folder: params?.folder
				}
			}
		})
	);
}

export async function createAsset(body: CreateAssetRequest): Promise<AssetSummary> {
	return unwrap(await client.POST('/api/v1/assets', { body }));
}

export interface GetAssetParams {
	page?: number;
	perPage?: number;
}

export async function getAsset(id: string, params?: GetAssetParams): Promise<AssetDetail> {
	return unwrap(
		await client.GET('/api/v1/assets/{id}', {
			params: {
				path: { id },
				query: { page: params?.page ?? 0, per_page: params?.perPage ?? 200 }
			}
		})
	);
}

export async function putAssetRecords(
	id: string,
	body: ReplaceRecordsRequest
): Promise<AssetSummary> {
	return unwrap(
		await client.PUT('/api/v1/assets/{id}/records', { params: { path: { id } }, body })
	);
}

/**
 * Reverse lineage (docs/20 §9): the runs (workflow instances) that pinned this
 * asset, newest first. Asset-level only — "which runs used asset X". Record /
 * material-level lineage is a deferred follow-on.
 */
export async function getAssetUsage(
	id: string,
	params?: { page?: number; perPage?: number }
): Promise<PaginatedAssetUsage> {
	return unwrap(
		await client.GET('/api/v1/assets/{id}/usage', {
			params: {
				path: { id },
				query: { page: params?.page ?? 1, per_page: params?.perPage ?? 20 }
			}
		})
	);
}

export async function deleteAsset(id: string): Promise<void> {
	const res = await client.DELETE('/api/v1/assets/{id}', { params: { path: { id } } });
	if (res.response.status >= 400) {
		throw new ApiError(
			res.response.status,
			res.error as Record<string, unknown> | string | undefined
		);
	}
}

// ── Multipart endpoints (file upload + CSV import) ───────────────────────────

async function parseErrorBody(res: Response): Promise<Record<string, unknown> | string> {
	const text = await res.text();
	try {
		return JSON.parse(text) as Record<string, unknown>;
	} catch {
		return text;
	}
}

/**
 * Upload a file for a `File` field on `asset`. Returns the S3 storage path to
 * drop into the record's File-field value (`InputSource::StoragePath`).
 */
export async function uploadAssetFile(
	assetId: string,
	field: string,
	file: File
): Promise<AssetFileUploadResponse> {
	const formData = new FormData();
	formData.append('file', file);
	const res = await authFetch(
		`${API_BASE}/assets/${assetId}/files?field=${encodeURIComponent(field)}`,
		{ method: 'POST', body: formData }
	);
	if (!res.ok) {
		throw new ApiError(res.status, await parseErrorBody(res));
	}
	return res.json() as Promise<AssetFileUploadResponse>;
}

export interface ImportCsvParams {
	hasHeader?: boolean;
	append?: boolean;
}

/**
 * Import a CSV file into `asset`. Columns map to type fields (by header name
 * when `hasHeader`, else positionally). Bumps the asset version.
 */
export async function importAssetCsv(
	assetId: string,
	file: File,
	params?: ImportCsvParams
): Promise<AssetSummary> {
	const formData = new FormData();
	formData.append('file', file);
	const qs = new URLSearchParams();
	qs.set('has_header', String(params?.hasHeader ?? true));
	qs.set('append', String(params?.append ?? false));
	const res = await authFetch(`${API_BASE}/assets/${assetId}/import-csv?${qs.toString()}`, {
		method: 'POST',
		body: formData
	});
	if (!res.ok) {
		throw new ApiError(res.status, await parseErrorBody(res));
	}
	return res.json() as Promise<AssetSummary>;
}
