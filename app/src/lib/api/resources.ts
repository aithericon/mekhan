/**
 * Typed wrappers for the Phase B.9 Resource CRUD endpoints.
 *
 * Mirrors the shape of `$lib/api/client.ts` — every function uses the same
 * `openapi-fetch` client (well, a fresh instance — see note below), surfaces
 * the older "throws on non-2xx" contract, and re-exports
 * `components['schemas'][...]` aliases so call sites don't reach into
 * `schema.d.ts` directly.
 *
 * Why a separate file rather than extending `client.ts`: `client.ts` is
 * already 800+ lines covering the older endpoints; the Phase B Resource
 * surface is a self-contained chunk and the user has parallel work on the
 * monolithic client. Keeping this file independent avoids merge friction.
 * Both files use the same generated `paths` / `components` types so they
 * stay in lockstep automatically.
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

export type ResourceSummary = components['schemas']['ResourceSummary'];
export type ResourceDetail = components['schemas']['ResourceDetail'];
export type ResourceTypeInfo = components['schemas']['ResourceTypeInfo'];
export type CreateResourceRequest = components['schemas']['CreateResourceRequest'];
export type UpdateResourceRequest = components['schemas']['UpdateResourceRequest'];
export type RotateResourceRequest = components['schemas']['RotateResourceRequest'];
export type ResourceAuditEntry = components['schemas']['ResourceAuditEntry'];
export type RepairPoolResponse = components['schemas']['RepairPoolResponse'];
export type PaginatedResources = components['schemas']['PaginatedResponse_ResourceSummary'];
export type PaginatedResourceAudit = components['schemas']['PaginatedResponse_ResourceAuditEntry'];

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

// ── Endpoints ──────────────────────────────────────────────────────────────

export interface ListResourcesParams {
	page?: number;
	perPage?: number;
	resource_type?: string;
	workspace_id?: string;
	/** Placement scope token: `workspace`, `folder:<uuid>`, or `template:<uuid>`.
	 *  When set, returns the downward-visible (most-specific-wins) set for that
	 *  context instead of the flat workspace list. */
	scope?: string;
	/** Placement filter: with `scope`, return only resources owned by EXACTLY
	 *  that scope (the browser), not the downward-visible set (the node picker). */
	exact?: boolean;
}

export async function listResources(params?: ListResourcesParams): Promise<PaginatedResources> {
	return unwrap(
		await client.GET('/api/v1/resources', {
			params: {
				query: {
					page: params?.page ?? 1,
					per_page: params?.perPage ?? 20,
					resource_type: params?.resource_type,
					workspace_id: params?.workspace_id,
					scope: params?.scope,
					exact: params?.exact
				}
			}
		})
	);
}

export async function listResourceTypes(): Promise<ResourceTypeInfo[]> {
	return unwrap(await client.GET('/api/v1/resources/types', {})) as ResourceTypeInfo[];
}

export async function createResource(body: CreateResourceRequest): Promise<ResourceSummary> {
	return unwrap(await client.POST('/api/v1/resources', { body }));
}

export async function getResource(id: string): Promise<ResourceDetail> {
	return unwrap(await client.GET('/api/v1/resources/{id}', { params: { path: { id } } }));
}

export async function updateResource(
	id: string,
	body: UpdateResourceRequest
): Promise<ResourceSummary> {
	return unwrap(
		await client.PUT('/api/v1/resources/{id}', { params: { path: { id } }, body })
	);
}

/** Reparent a resource to a different owner scope (`PATCH …/{id}/scope`). The
 *  `scope` token is `workspace`, `folder:<uuid>`, or `template:<uuid>`. */
export async function moveResource(id: string, scope: string): Promise<ResourceSummary> {
	const [kind, sid] = scope.split(':');
	const body =
		kind === 'workspace' || !sid
			? { scope_kind: 'workspace' as const }
			: { scope_kind: kind as 'folder' | 'template', scope_id: sid };
	return unwrap(
		await client.PATCH('/api/v1/resources/{id}/scope', { params: { path: { id } }, body })
	);
}

export async function deleteResource(id: string): Promise<void> {
	const res = await client.DELETE('/api/v1/resources/{id}', { params: { path: { id } } });
	if (res.response.status >= 400) {
		const detail = res.error ? JSON.stringify(res.error) : '';
		throw new Error(`API error ${res.response.status}: ${detail}`);
	}
}

/** Operator recovery for a pool (`capacity`) resource whose backing engine net
 *  was lost or drifted (`POST …/{id}/repair`). Re-deploys the pool net and
 *  re-arms live presence so capacity re-establishes on the next heartbeat.
 *  Idempotent — safe to run on a healthy pool. */
export async function repairPool(id: string): Promise<RepairPoolResponse> {
	return unwrap(
		await client.POST('/api/v1/resources/{id}/repair', { params: { path: { id } } })
	);
}

export async function rotateResource(
	id: string,
	body: RotateResourceRequest
): Promise<ResourceSummary> {
	return unwrap(
		await client.POST('/api/v1/resources/{id}/rotate', { params: { path: { id } }, body })
	);
}

export async function listResourceAudit(
	id: string,
	params?: { page?: number; perPage?: number }
): Promise<PaginatedResourceAudit> {
	return unwrap(
		await client.GET('/api/v1/resources/{id}/audit', {
			params: {
				path: { id },
				query: {
					page: params?.page ?? 1,
					per_page: params?.perPage ?? 20
				}
			}
		})
	);
}
