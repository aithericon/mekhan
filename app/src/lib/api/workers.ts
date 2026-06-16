/**
 * Typed wrapper for the worker-pool coverage endpoint.
 *
 * The worker pool is a set of anonymous, competing-consumer executor workers
 * (NOT enrolled runners — see `$lib/api/runners.ts` for the presence-pool /
 * instrument fleet). Each worker advertises which `ExecutorJob` backends it
 * serves; this read surfaces live worker presence + per-backend coverage so an
 * operator can see which backends have zero live workers.
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

export type WorkerCoverageResponse = components['schemas']['WorkerCoverageResponse'];
export type WorkerCoverageEntry = components['schemas']['WorkerCoverageEntry'];
export type BackendCoverageEntry = components['schemas']['BackendCoverageEntry'];
export type WorkerSummary = components['schemas']['WorkerSummary'];
export type CreatedWorkerRegistrationToken = components['schemas']['CreatedWorkerRegistrationToken'];
export type CreateWorkerRegistrationTokenRequest =
	components['schemas']['CreateWorkerRegistrationTokenRequest'];
export type PaginatedWorkers = components['schemas']['PaginatedResponse_WorkerSummary'];

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

// ── Coverage endpoint ────────────────────────────────────────────────────────

/**
 * GET /api/v1/workers/coverage — live worker-pool coverage snapshot.
 * Returns connected workers (with their advertised backends + heartbeat
 * freshness) and per-backend coverage across every `ExecutorJob` backend
 * (`worker_count === 0` means the backend is uncovered — jobs will queue).
 */
export async function getWorkerCoverage(): Promise<WorkerCoverageResponse> {
	return unwrap(await client.GET('/api/v1/workers/coverage', {}));
}

// ── Worker list endpoint ───────────────────────────────────────────────────────

export interface ListWorkersParams {
	page?: number;
	perPage?: number;
	/** List the shared platform-tier worker pool instead of the caller's workspace. */
	platform?: boolean;
}

/** GET /api/v1/workers — paginated list of enrolled workers (caller's workspace,
 * or the shared platform pool when `platform` is set). */
export async function listWorkers(params?: ListWorkersParams): Promise<PaginatedWorkers> {
	return unwrap(
		await client.GET('/api/v1/workers', {
			params: {
				query: {
					page: params?.page ?? 1,
					per_page: params?.perPage ?? 200,
					platform: params?.platform ?? false
				}
			}
		})
	);
}

// ── Registration-token endpoint ────────────────────────────────────────────────

/**
 * POST /api/v1/workers/registration-tokens — mint a new `wt_` token.
 * The returned `token` field is shown exactly once; it is never re-served.
 */
export async function createWorkerRegistrationToken(
	body: CreateWorkerRegistrationTokenRequest
): Promise<CreatedWorkerRegistrationToken> {
	return unwrap(await client.POST('/api/v1/workers/registration-tokens', { body }));
}
