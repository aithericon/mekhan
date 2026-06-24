/**
 * Typed wrappers for the invite lifecycle (Phase 4). Admin endpoints
 * (create/list/resend/revoke) go through the session-cookie client; the PUBLIC
 * preview/accept endpoints use a bare client WITHOUT the 401-redirect
 * middleware, since an invitee may have no session yet (a 401 there must not
 * bounce them to login — the endpoints are authed by the opaque token).
 *
 * Mirrors `$lib/api/roster.ts`: same `openapi-fetch` client, same `unwrap`,
 * types aliased from the generated `schema.d.ts`.
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

// Public client: no redirect middleware (an unauthenticated invitee uses it).
const publicClient = createClient<paths>({ baseUrl: '', credentials: 'same-origin' });

// ── Types ─────────────────────────────────────────────────────────────────

export type InviteSummary = components['schemas']['InviteSummary'];
export type InvitePreview = components['schemas']['InvitePreview'];
export type CreateInviteRequest = components['schemas']['CreateInviteRequest'];
export type AcceptInviteResponse = components['schemas']['AcceptInviteResponse'];
export type InviteObjectGrantSpec = components['schemas']['InviteObjectGrantSpec'];

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

// ── Admin endpoints ─────────────────────────────────────────────────────────

/** GET /api/v1/workspaces/{id}/invites — list invites (Admin). */
export async function listInvites(workspaceId: string): Promise<InviteSummary[]> {
	return unwrap(
		await client.GET('/api/v1/workspaces/{id}/invites', {
			params: { path: { id: workspaceId } }
		})
	);
}

/** POST /api/v1/workspaces/{id}/invites — create/rotate an invite (Admin). */
export async function createInvite(
	workspaceId: string,
	body: CreateInviteRequest
): Promise<InviteSummary> {
	return unwrap(
		await client.POST('/api/v1/workspaces/{id}/invites', {
			params: { path: { id: workspaceId } },
			body
		})
	);
}

/** POST /api/v1/workspaces/{id}/invites/{invite_id}/resend — rotate + resend. */
export async function resendInvite(
	workspaceId: string,
	inviteId: string
): Promise<InviteSummary> {
	return unwrap(
		await client.POST('/api/v1/workspaces/{id}/invites/{invite_id}/resend', {
			params: { path: { id: workspaceId, invite_id: inviteId } }
		})
	);
}

/** DELETE /api/v1/workspaces/{id}/invites/{invite_id} — revoke. */
export async function revokeInvite(workspaceId: string, inviteId: string): Promise<void> {
	const res = await client.DELETE('/api/v1/workspaces/{id}/invites/{invite_id}', {
		params: { path: { id: workspaceId, invite_id: inviteId } }
	});
	if (res.response.status >= 400) {
		const detail = res.error ? JSON.stringify(res.error) : '';
		throw new Error(`API error ${res.response.status}: ${detail}`);
	}
}

// ── Invitee endpoints ─────────────────────────────────────────────────────────

/**
 * GET /api/v1/invites/{token}/preview — PUBLIC (the accept page shows the
 * workspace/role/email before login). Uses the bare client so a not-yet-signed
 * in invitee isn't bounced. Throws on 404 (invalid).
 */
export async function previewInvite(token: string): Promise<InvitePreview> {
	return unwrap(
		await publicClient.GET('/api/v1/invites/{token}/preview', {
			params: { path: { token } }
		})
	);
}

/**
 * POST /api/v1/invites/{token}/accept — AUTHED: the logged-in session IS the
 * joining identity. Uses the session-cookie client; a 401 here means no session
 * and the middleware bounces to `/api/auth/login?return_to=<this accept URL>`,
 * so the invitee returns to the accept page after signing in.
 */
export async function acceptInvite(token: string): Promise<AcceptInviteResponse> {
	return unwrap(
		await client.POST('/api/v1/invites/{token}/accept', {
			params: { path: { token } }
		})
	);
}
