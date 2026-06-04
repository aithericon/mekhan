/**
 * Typed wrappers for the Phase 1–5 Runner endpoints.
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

export type RunnerSummary = components['schemas']['RunnerSummary'];
export type RunnerDetail = components['schemas']['RunnerDetail'];
export type RunnerPresenceSnapshot = components['schemas']['RunnerPresenceSnapshot'];
export type RegistrationTokenSummary = components['schemas']['RegistrationTokenSummary'];
export type CreatedRegistrationToken = components['schemas']['CreatedRegistrationToken'];
export type CreateRegistrationTokenRequest =
	components['schemas']['CreateRegistrationTokenRequest'];
export type PaginatedRunners = components['schemas']['PaginatedResponse_RunnerSummary'];
export type PaginatedRegistrationTokens =
	components['schemas']['PaginatedResponse_RegistrationTokenSummary'];
export type RunnerInterfaces = components['schemas']['RunnerInterfaces'];
export type RunnerInterfaceCatalog = components['schemas']['RunnerInterfaceCatalog'];
export type InterfaceEntry = components['schemas']['InterfaceEntry'];

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

// ── Runner endpoints ───────────────────────────────────────────────────────

export interface ListRunnersParams {
	page?: number;
	perPage?: number;
}

/** GET /api/v1/runners — paginated, workspace-scoped list of live runners. */
export async function listRunners(params?: ListRunnersParams): Promise<PaginatedRunners> {
	return unwrap(
		await client.GET('/api/v1/runners', {
			params: {
				query: {
					page: params?.page ?? 1,
					per_page: params?.perPage ?? 20
				}
			}
		})
	);
}

/** GET /api/v1/runners/{id} — admin detail view of a single runner. */
export async function getRunner(id: string): Promise<RunnerDetail> {
	return unwrap(await client.GET('/api/v1/runners/{id}', { params: { path: { id } } }));
}

/** DELETE /api/v1/runners/{id} — revoke (soft delete, status → 'revoked'). */
export async function revokeRunner(id: string): Promise<void> {
	const res = await client.DELETE('/api/v1/runners/{id}', { params: { path: { id } } });
	if (res.response.status >= 400) {
		const detail = res.error ? JSON.stringify(res.error) : '';
		throw new Error(`API error ${res.response.status}: ${detail}`);
	}
}

// ── Registration-token endpoints ──────────────────────────────────────────

export interface ListRegistrationTokensParams {
	page?: number;
	perPage?: number;
}

/** GET /api/v1/runners/registration-tokens — paginated, workspace-scoped. */
export async function listRegistrationTokens(
	params?: ListRegistrationTokensParams
): Promise<PaginatedRegistrationTokens> {
	return unwrap(
		await client.GET('/api/v1/runners/registration-tokens', {
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
 * POST /api/v1/runners/registration-tokens — mint a new `rt_` token.
 * The returned `token` field is shown exactly once; it is never re-served.
 */
export async function createRegistrationToken(
	body: CreateRegistrationTokenRequest
): Promise<CreatedRegistrationToken> {
	return unwrap(await client.POST('/api/v1/runners/registration-tokens', { body }));
}

/** DELETE /api/v1/runners/registration-tokens/{id} — soft-revoke a token. */
export async function revokeRegistrationToken(id: string): Promise<void> {
	const res = await client.DELETE('/api/v1/runners/registration-tokens/{id}', {
		params: { path: { id } }
	});
	if (res.response.status >= 400) {
		const detail = res.error ? JSON.stringify(res.error) : '';
		throw new Error(`API error ${res.response.status}: ${detail}`);
	}
}

// ── Presence endpoint ──────────────────────────────────────────────────────

/**
 * GET /api/v1/runners/presence — live in-memory presence snapshot.
 * Reflects the presence-controller's PresenceMap (the actual pool-capacity
 * signal), NOT `runners.last_seen_at` (a best-effort UI bump).
 */
export async function getRunnerPresence(): Promise<RunnerPresenceSnapshot[]> {
	return unwrap(await client.GET('/api/v1/runners/presence', {}));
}

// ── Interface-catalog endpoint ─────────────────────────────────────────────

/**
 * GET /api/v1/runners/{id}/interfaces — the runner's self-reported interface
 * catalog (ROS topics/services/actions). Returns `null` when the runner has
 * never pushed a catalog (the endpoint replies 404), so callers can render a
 * friendly empty state instead of treating it as an error.
 */
export async function getRunnerInterfaces(id: string): Promise<RunnerInterfaces | null> {
	const res = await client.GET('/api/v1/runners/{id}/interfaces', {
		params: { path: { id } }
	});
	if (res.response.status === 404) return null;
	return unwrap(res);
}

// ── ROS interface aggregation ──────────────────────────────────────────────

/** One runner's ROS interface catalog, tagged with the runner it came from. */
export interface RosInterfaceSource {
	runnerId: string;
	runnerName: string;
	catalog: RunnerInterfaceCatalog;
}

/**
 * List ROS interface catalogs across runners advertising a `ros` capability.
 *
 * Pulls the runner list (one page, up to 100), keeps those whose `capabilities`
 * object carries a `ros` key, then fetches each one's self-reported interface
 * catalog. Runners that have never pushed a catalog (404 → `null`) are skipped.
 */
export async function listRosInterfaces(): Promise<RosInterfaceSource[]> {
	const { items } = await listRunners({ perPage: 100 });
	const rosRunners = items.filter(
		(r) =>
			r.capabilities &&
			typeof r.capabilities === 'object' &&
			'ros' in (r.capabilities as Record<string, unknown>)
	);
	const sources = await Promise.all(
		rosRunners.map(async (r) => {
			const ifaces = await getRunnerInterfaces(r.id);
			if (!ifaces) return null;
			return { runnerId: r.id, runnerName: r.name, catalog: ifaces.catalog };
		})
	);
	return sources.filter((s): s is RosInterfaceSource => s !== null);
}
