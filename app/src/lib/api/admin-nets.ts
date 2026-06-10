/**
 * Typed wrappers for the admin engine-net overview + kill-switch / cleanup
 * endpoints (2026-06-10 incident follow-up). Mirrors `$lib/api/roster.ts`:
 * same `openapi-fetch` client pattern, same `unwrap()` helper, types derived
 * from the generated `schema.d.ts`.
 *
 * All three endpoints require workspace Admin; `listAdminNets` throws an
 * `AdminNetsForbidden` on 403 so the Engine Nets page can fall back to the
 * read-only `/petri/api/nets/metadata` view for non-admins.
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

export type AdminNetRow = components['schemas']['AdminNetRow'];
export type PurgeEventsResponse = components['schemas']['PurgeEventsResponse'];
export type BulkKillResponse = components['schemas']['BulkKillResponse'];
export type PurgeTerminalResponse = components['schemas']['PurgeTerminalResponse'];

/** 403 from the admin list — the caller is not a workspace admin. */
export class AdminNetsForbidden extends Error {
	constructor() {
		super('admin role required');
	}
}

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

// ── Endpoints ────────────────────────────────────────────────────────────────

/**
 * GET /api/v1/admin/nets — engine-wide net overview with per-net PETRI_GLOBAL
 * event counts (sorted highest first: runaways float to the top) and the
 * owning instance join. Throws `AdminNetsForbidden` on 403.
 */
export async function listAdminNets(): Promise<AdminNetRow[]> {
	const result = await client.GET('/api/v1/admin/nets');
	if (result.response.status === 403) throw new AdminNetsForbidden();
	return unwrap(result);
}

/**
 * DELETE /api/v1/admin/nets/{net_id} — the kill switch. Engine-side proper
 * terminate: lease-finalizer drain + `NetCancelled` + task cancellation.
 * Idempotent.
 */
export async function killNet(netId: string): Promise<void> {
	const result = await client.DELETE('/api/v1/admin/nets/{net_id}', {
		params: { path: { net_id: netId } }
	});
	if (result.error !== undefined) {
		throw new Error(`API error ${result.response.status}: ${JSON.stringify(result.error)}`);
	}
}

/**
 * POST /api/v1/admin/nets/{net_id}/purge-events — cleanup: purge the net's
 * event + signal subjects from PETRI_GLOBAL. 409 while the net is active.
 */
export async function purgeNetEvents(netId: string): Promise<PurgeEventsResponse> {
	const result = await client.POST('/api/v1/admin/nets/{net_id}/purge-events', {
		params: { path: { net_id: netId } }
	});
	return unwrap(result);
}

/**
 * POST /api/v1/admin/nets/bulk-kill — terminate many nets at once. Infra
 * (non-`mekhan-`) nets in `netIds` are skipped server-side unless
 * `includeInfrastructure` is set. Per-net failures are reported, not fatal.
 */
export async function bulkKillNets(
	netIds: string[],
	includeInfrastructure = false
): Promise<BulkKillResponse> {
	const result = await client.POST('/api/v1/admin/nets/bulk-kill', {
		body: { net_ids: netIds, include_infrastructure: includeInfrastructure }
	});
	return unwrap(result);
}

/**
 * POST /api/v1/admin/nets/purge-terminal — sweep every terminal net's events
 * out of PETRI_GLOBAL in one shot. Active nets are never touched.
 */
export async function purgeTerminalNets(): Promise<PurgeTerminalResponse> {
	const result = await client.POST('/api/v1/admin/nets/purge-terminal', {});
	return unwrap(result);
}
