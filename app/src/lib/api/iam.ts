/**
 * Typed wrappers for the Phase-3 object-grant ACL + member-role endpoints.
 *
 * Mirrors `$lib/api/roster.ts` / `$lib/api/invites.ts`: the same `openapi-fetch`
 * client with the 401→login middleware, the same `unwrap()`, types aliased from
 * the generated `schema.d.ts`. (Invites have their own module already.)
 *
 * The nine grant routes are three verbs over three concrete object kinds
 * (`folders|templates|instances`); the backend models them as path literals so
 * utoipa types them, so a single `objectType` discriminator here selects the
 * path without any string interpolation into the typed client.
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

// ── Types ─────────────────────────────────────────────────────────────────

export type GrantView = components['schemas']['GrantView'];
export type PutGrantRequest = components['schemas']['PutGrantRequest'];

/** The object kinds the grant routes are defined for. */
export type ObjectType = 'folder' | 'template' | 'instance' | 'resource' | 'asset';

/** Effective-role rank for client-side gating. Higher = more privilege.
 *  Mirrors the backend `Role` ordering. */
const ROLE_RANK: Record<string, number> = { viewer: 0, editor: 1, admin: 2, owner: 3 };

/** `true` when `have` (an effective role label, or null) meets `need`. */
export function roleAtLeast(have: string | null | undefined, need: string): boolean {
	if (!have) return false;
	return (ROLE_RANK[have] ?? -1) >= (ROLE_RANK[need] ?? Infinity);
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

// Path tables keyed by object kind — the typed client needs literal path
// strings, so we map the discriminator to the right pair.
const LIST_PATHS = {
	folder: '/api/v1/folders/{id}/grants',
	template: '/api/v1/templates/{id}/grants',
	instance: '/api/v1/instances/{id}/grants',
	resource: '/api/v1/resources/{id}/grants',
	asset: '/api/v1/assets/{id}/grants'
} as const;
const ITEM_PATHS = {
	folder: '/api/v1/folders/{id}/grants/{user_id}',
	template: '/api/v1/templates/{id}/grants/{user_id}',
	instance: '/api/v1/instances/{id}/grants/{user_id}',
	resource: '/api/v1/resources/{id}/grants/{user_id}',
	asset: '/api/v1/assets/{id}/grants/{user_id}'
} as const;

// ── Object grants ────────────────────────────────────────────────────────────

/** GET .../{id}/grants — the FULL effective access list (direct object grants +
 *  inherited folder grants + workspace-member floor), each tagged with `source`.
 *  Object-Admin (or workspace Admin/Owner via bypass) only — 403 otherwise. */
export async function listGrants(objectType: ObjectType, id: string): Promise<GrantView[]> {
	return unwrap(
		await client.GET(LIST_PATHS[objectType], { params: { path: { id } } })
	) as GrantView[];
}

/** PUT .../{id}/grants/{user_id} — upsert a direct object grant for a member.
 *  409 if the grantee isn't a workspace member; 403 on escalation past the
 *  caller's own effective role. Returns the persisted (source==='object') row. */
export async function putGrant(
	objectType: ObjectType,
	id: string,
	userId: string,
	role: string
): Promise<GrantView> {
	return unwrap(
		await client.PUT(ITEM_PATHS[objectType], {
			params: { path: { id, user_id: userId } },
			body: { role }
		})
	) as GrantView;
}

/** DELETE .../{id}/grants/{user_id} — drop a direct object grant. The member
 *  falls back to their inherited / workspace-floor role. 204 on success. */
export async function deleteGrant(
	objectType: ObjectType,
	id: string,
	userId: string
): Promise<void> {
	const res = await client.DELETE(ITEM_PATHS[objectType], {
		params: { path: { id, user_id: userId } }
	});
	if (res.response.status >= 400) {
		const detail = res.error ? JSON.stringify(res.error) : '';
		throw new Error(`API error ${res.response.status}: ${detail}`);
	}
}

// ── Workspace member role ──────────────────────────────────────────────────────

/** PATCH /api/v1/workspaces/{id}/members/{user_id} — change a member's
 *  workspace role. 409 when it would demote the last remaining owner (the
 *  server is authoritative; the SPA also guards this for UX). */
export async function updateMemberRole(
	workspaceId: string,
	userId: string,
	role: string
): Promise<components['schemas']['WorkspaceMember']> {
	return unwrap(
		await client.PATCH('/api/v1/workspaces/{id}/members/{user_id}', {
			params: { path: { id: workspaceId, user_id: userId } },
			body: { role }
		})
	);
}
