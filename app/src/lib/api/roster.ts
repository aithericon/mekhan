/**
 * Typed wrappers for the human-capacity roster + presence endpoints
 * (docs/33 — Humans as a Capacity). The human counterpart to `$lib/api/runners.ts`.
 *
 * Mirrors that module's shape: the same `openapi-fetch` client instance pattern,
 * the same `unwrap()` helper, the same `components['schemas'][...]` type aliases.
 * All paths are derived from the generated `schema.d.ts`.
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

export type RosterMemberDetail = components['schemas']['RosterMemberDetail'];
export type RosterMemberSummary = components['schemas']['RosterMemberSummary'];
export type AvailabilityConfig = components['schemas']['AvailabilityConfig'];
export type HumanPresenceSnapshot = components['schemas']['HumanPresenceSnapshot'];

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

// ── Roster endpoints ─────────────────────────────────────────────────────────

/**
 * GET /api/v1/roster/me — the caller's OWN live enrollments across the workspace.
 * Feeds the availability toggle (one switch per enrolled human capacity).
 */
export async function getMyEnrollments(): Promise<RosterMemberDetail[]> {
	return unwrap(await client.GET('/api/v1/roster/me', {}));
}

/**
 * POST /api/v1/roster/availability — flip the caller's durable availability intent
 * on one human capacity. The presence controller wakes on the published edge.
 */
export async function setAvailability(capacityId: string, available: boolean): Promise<void> {
	const res = await client.POST('/api/v1/roster/availability', {
		body: { capacity_id: capacityId, available }
	});
	if (res.response.status >= 400) {
		const detail = res.error ? JSON.stringify(res.error) : '';
		throw new Error(`API error ${res.response.status}: ${detail}`);
	}
}

/**
 * GET /api/v1/human-presence — live in-memory snapshot of which enrolled members
 * are currently ADMITTED to their pools (the Fleet human-pool view). Distinct from
 * the durable `available` intent on the roster row.
 */
export async function getHumanPresence(): Promise<HumanPresenceSnapshot[]> {
	return unwrap(await client.GET('/api/v1/human-presence', {}));
}

/**
 * GET /api/v1/roster — workspace-scoped list of enrolled members, optionally
 * filtered to one human capacity. Live-only (revoked excluded).
 */
export async function listRoster(capacityId?: string): Promise<RosterMemberSummary[]> {
	const data = unwrap(
		await client.GET('/api/v1/roster', {
			params: { query: capacityId ? { capacity_id: capacityId } : {} }
		})
	) as { items?: RosterMemberSummary[] };
	return data.items ?? [];
}

/**
 * POST /api/v1/roster — admin-enroll a workspace member into a human capacity.
 * Caps are validated against the workspace's `CapabilityType`s (400 on mismatch);
 * a repeat (capacity, member) enrollment → 409. Returns the full detail row.
 */
export async function enrollMember(input: {
	capacity_id: string;
	member_user_id: string;
	concurrency?: number;
	caps?: Record<string, unknown>;
	availability?: AvailabilityConfig;
}): Promise<RosterMemberDetail> {
	return unwrap(await client.POST('/api/v1/roster', { body: input }));
}

/**
 * PATCH /api/v1/roster/{id} — admin update of a member's caps / concurrency /
 * availability. Every field optional; only the supplied ones are written. When
 * `caps` is supplied it is re-validated against the registry (400 on mismatch).
 */
export async function updateRosterMember(
	id: string,
	input: {
		concurrency?: number;
		caps?: Record<string, unknown>;
		availability?: AvailabilityConfig;
	}
): Promise<RosterMemberDetail> {
	return unwrap(
		await client.PATCH('/api/v1/roster/{id}', {
			params: { path: { id } },
			body: input
		})
	);
}

/**
 * DELETE /api/v1/roster/{id} — revoke a member (soft delete; sets `revoked_at`).
 * 204 No Content on success.
 */
export async function revokeMember(id: string): Promise<void> {
	const res = await client.DELETE('/api/v1/roster/{id}', {
		params: { path: { id } }
	});
	if (res.response.status >= 400) {
		const detail = res.error ? JSON.stringify(res.error) : '';
		throw new Error(`API error ${res.response.status}: ${detail}`);
	}
}
