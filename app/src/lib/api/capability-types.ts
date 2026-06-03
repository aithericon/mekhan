/**
 * Typed wrappers for the Phase 4 Capability-Type endpoints.
 *
 * Mirrors the shape of `$lib/api/resources.ts` — same `openapi-fetch` client
 * instance pattern, same `unwrap()` helper, same `components['schemas'][...]`
 * type aliases. All paths are derived from the generated `schema.d.ts`.
 */
import createClient, { type Middleware } from 'openapi-fetch';
import type { components, paths } from './schema';

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

export type FieldKind = components['schemas']['FieldKind'];
export type CapabilityField = components['schemas']['CapabilityField'];
export type CapabilityTypeSummary = components['schemas']['CapabilityTypeSummary'];
export type CapabilityTypeDetail = components['schemas']['CapabilityTypeDetail'];
export type CreateCapabilityTypeRequest = components['schemas']['CreateCapabilityTypeRequest'];
export type PaginatedCapabilityTypes =
	components['schemas']['PaginatedResponse_CapabilityTypeSummary'];

// ── Helpers ────────────────────────────────────────────────────────────────

function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
	if (result.error !== undefined) {
		const status = result.response.status;
		const body =
			typeof result.error === 'object' ? JSON.stringify(result.error) : String(result.error);
		throw new Error(`API error ${status}: ${body}`);
	}
	if (result.data === undefined) {
		throw new Error(`API error ${result.response.status}: empty body`);
	}
	return result.data;
}

// ── Capability-type endpoints ──────────────────────────────────────────────

export interface ListCapabilityTypesParams {
	page?: number;
	perPage?: number;
}

/** GET /api/v1/capability-types — paginated, workspace-scoped list. */
export async function listCapabilityTypes(
	params?: ListCapabilityTypesParams
): Promise<PaginatedCapabilityTypes> {
	return unwrap(
		await client.GET('/api/v1/capability-types', {
			params: {
				query: {
					page: params?.page ?? 1,
					per_page: params?.perPage ?? 20
				}
			}
		})
	);
}

/**
 * POST /api/v1/capability-types — mint a capability type.
 * Cookie-only (browser admin boundary). Returns the compact summary row.
 */
export async function createCapabilityType(
	body: CreateCapabilityTypeRequest
): Promise<CapabilityTypeSummary> {
	return unwrap(await client.POST('/api/v1/capability-types', { body }));
}

/** GET /api/v1/capability-types/{id} — admin detail view. */
export async function getCapabilityType(id: string): Promise<CapabilityTypeDetail> {
	return unwrap(
		await client.GET('/api/v1/capability-types/{id}', { params: { path: { id } } })
	);
}

/**
 * DELETE /api/v1/capability-types/{id} — soft revoke (204 No Content on
 * success; existing runners that advertise the capability are unaffected).
 */
export async function revokeCapabilityType(id: string): Promise<void> {
	const res = await client.DELETE('/api/v1/capability-types/{id}', {
		params: { path: { id } }
	});
	if (res.response.status >= 400) {
		const detail = res.error ? JSON.stringify(res.error) : '';
		throw new Error(`API error ${res.response.status}: ${detail}`);
	}
}
