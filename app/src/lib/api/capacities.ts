/**
 * Typed wrapper for the unified Control-Plane read endpoint.
 *
 * `GET /api/v1/capacities` returns every `capacity` + `datacenter` resource in
 * the workspace, classified by its dispatch backend (the SINGLE authority
 * `CapacityAxes::backend()`) and decorated with live utilization (`CapacityLive`,
 * tagged by backend). This is the read side of the Control Plane — one row per
 * capacity, partitioned into the four backend sections (Presence / Queue /
 * Tokens / Scheduler).
 *
 * Same `openapi-fetch` client + `unwrap()` pattern as `$lib/api/runners.ts`.
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

/** One capacity resource, classified + live. The unified Control-Plane row. */
export type CapacitySummary = components['schemas']['CapacitySummary'];
/** The dispatch target, from `CapacityAxes::backend()`. */
export type CapacityBackend = components['schemas']['CapacityBackend'];
/** Live utilization for one capacity, tagged by its backend. */
export type CapacityLive = components['schemas']['CapacityLive'];
/** The typed point in trait-space a `capacity` resource names. */
export type CapacityAxes = components['schemas']['CapacityAxes'];

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

// ── Capacities endpoint ──────────────────────────────────────────────────────

/**
 * GET /api/v1/capacities — every `capacity` + `datacenter` resource in the
 * workspace, classified by backend + decorated with live utilization. The
 * Control Plane partitions the list into its four backend sections.
 */
export async function listCapacities(): Promise<CapacitySummary[]> {
	return unwrap(await client.GET('/api/v1/capacities', {}));
}
