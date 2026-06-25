/**
 * Typed wrapper for the platform-admin JetStream introspection endpoints.
 *
 *   GET /api/v1/admin/jetstream/streams                  — list + counts
 *   GET /api/v1/admin/jetstream/streams/{name}           — detail + consumers
 *   GET /api/v1/admin/jetstream/streams/{name}/messages  — non-destructive peek
 *
 * Read-only debug surface — gated server-side on `is_platform_admin`. Same
 * `openapi-fetch` client + `unwrap()` pattern as `$lib/api/capacities.ts`
 * (a fresh instance per wrapper file — see the note in `resources.ts`).
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

export type JsStreamSummary = components['schemas']['JsStreamSummary'];
export type JsConsumerSummary = components['schemas']['JsConsumerSummary'];
export type JsStreamDetail = components['schemas']['JsStreamDetail'];
export type JsMessage = components['schemas']['JsMessage'];
export type JsMessagesResponse = components['schemas']['JsMessagesResponse'];

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

// ── Endpoints ────────────────────────────────────────────────────────────────

/** Every JetStream stream with its headline counts, sorted by name. */
export async function listStreams(): Promise<JsStreamSummary[]> {
	return unwrap(await client.GET('/api/v1/admin/jetstream/streams', {}));
}

/** One stream's detail plus every consumer bound to it. */
export async function getStream(name: string): Promise<JsStreamDetail> {
	return unwrap(
		await client.GET('/api/v1/admin/jetstream/streams/{name}', {
			params: { path: { name } }
		})
	);
}

/**
 * Non-destructively peek raw messages, newest first. `beforeSeq` (the previous
 * page's `next_before_seq`) walks backwards into older messages; omit it for the
 * stream tail.
 */
export async function peekMessages(
	name: string,
	opts: { beforeSeq?: number; limit?: number } = {}
): Promise<JsMessagesResponse> {
	return unwrap(
		await client.GET('/api/v1/admin/jetstream/streams/{name}/messages', {
			params: {
				path: { name },
				query: { before_seq: opts.beforeSeq, limit: opts.limit }
			}
		})
	);
}
