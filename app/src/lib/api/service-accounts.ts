/**
 * Typed wrappers for workspace SERVICE ACCOUNTS — non-human, workspace-owned API
 * principals that survive member offboarding (additive to the human `uat_` PATs
 * managed under the profile Access Tokens card).
 *
 * Every endpoint is human-Admin-gated server-side; this client only hides
 * affordances. Mirrors `$lib/api/invites.ts`: same `openapi-fetch` client, same
 * session-expiry middleware + `unwrap`, types aliased from the generated
 * `schema.d.ts`.
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

export type ServiceAccountSummary = components['schemas']['ServiceAccountSummary'];
export type ServiceAccountTokenSummary = components['schemas']['ServiceAccountTokenSummary'];
export type CreateServiceAccountRequest = components['schemas']['CreateServiceAccountRequest'];
export type PatchServiceAccountRequest = components['schemas']['PatchServiceAccountRequest'];
export type CreateServiceAccountTokenRequest =
	components['schemas']['CreateServiceAccountTokenRequest'];
export type CreatedServiceAccountToken = components['schemas']['CreatedServiceAccountToken'];

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

function ensureOk(result: { error?: unknown; response: Response }): void {
	if (result.response.status >= 400) {
		const detail = result.error ? JSON.stringify(result.error) : '';
		throw new Error(`API error ${result.response.status}: ${detail}`);
	}
}

// ── Service accounts ────────────────────────────────────────────────────────

/** GET .../service-accounts — list ALL service accounts in the workspace. */
export async function listServiceAccounts(workspaceId: string): Promise<ServiceAccountSummary[]> {
	return unwrap(
		await client.GET('/api/v1/workspaces/{workspace_id}/service-accounts', {
			params: { path: { workspace_id: workspaceId } }
		})
	);
}

/** POST .../service-accounts — create a service account. */
export async function createServiceAccount(
	workspaceId: string,
	body: CreateServiceAccountRequest
): Promise<ServiceAccountSummary> {
	return unwrap(
		await client.POST('/api/v1/workspaces/{workspace_id}/service-accounts', {
			params: { path: { workspace_id: workspaceId } },
			body
		})
	);
}

/** PATCH .../service-accounts/{sa_id} — rename and/or toggle disabled state. */
export async function patchServiceAccount(
	workspaceId: string,
	serviceAccountId: string,
	body: PatchServiceAccountRequest
): Promise<ServiceAccountSummary> {
	return unwrap(
		await client.PATCH('/api/v1/workspaces/{workspace_id}/service-accounts/{sa_id}', {
			params: { path: { workspace_id: workspaceId, sa_id: serviceAccountId } },
			body
		})
	);
}

/** DELETE .../service-accounts/{sa_id} — delete (CASCADE drops its tokens). */
export async function deleteServiceAccount(
	workspaceId: string,
	serviceAccountId: string
): Promise<void> {
	ensureOk(
		await client.DELETE('/api/v1/workspaces/{workspace_id}/service-accounts/{sa_id}', {
			params: { path: { workspace_id: workspaceId, sa_id: serviceAccountId } }
		})
	);
}

// ── Service-account tokens ────────────────────────────────────────────────────

/** GET .../service-accounts/{sa_id}/tokens — token metadata (never the secret). */
export async function listServiceAccountTokens(
	workspaceId: string,
	serviceAccountId: string
): Promise<ServiceAccountTokenSummary[]> {
	return unwrap(
		await client.GET('/api/v1/workspaces/{workspace_id}/service-accounts/{sa_id}/tokens', {
			params: { path: { workspace_id: workspaceId, sa_id: serviceAccountId } }
		})
	);
}

/** POST .../service-accounts/{sa_id}/tokens — mint a token; secret shown once. */
export async function createServiceAccountToken(
	workspaceId: string,
	serviceAccountId: string,
	body: CreateServiceAccountTokenRequest
): Promise<CreatedServiceAccountToken> {
	return unwrap(
		await client.POST('/api/v1/workspaces/{workspace_id}/service-accounts/{sa_id}/tokens', {
			params: { path: { workspace_id: workspaceId, sa_id: serviceAccountId } },
			body
		})
	);
}

/** DELETE .../service-accounts/{sa_id}/tokens/{token_id} — revoke a token. */
export async function revokeServiceAccountToken(
	workspaceId: string,
	serviceAccountId: string,
	tokenId: string
): Promise<void> {
	ensureOk(
		await client.DELETE(
			'/api/v1/workspaces/{workspace_id}/service-accounts/{sa_id}/tokens/{token_id}',
			{
				params: {
					path: { workspace_id: workspaceId, sa_id: serviceAccountId, token_id: tokenId }
				}
			}
		)
	);
}
