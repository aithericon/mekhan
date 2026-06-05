/**
 * Typed wrapper for the model-pool control plane read + transition endpoints.
 *
 * `GET /api/v1/models` is the loaded-set projection — one `ModelSetView` row per
 * model the operator has curated into the pool, with `available` being the
 * AND-gate (`state == loaded` AND a LIVE runner advertises `model_id`). The
 * editor's internal-provider model picker filters on `available`.
 *
 * `POST /api/v1/models/{model_id}/transition` is the operator state-machine step
 * (`approved → loading → loaded → draining → unloaded`); an illegal edge → 409.
 *
 * Same `openapi-fetch` client + `unwrap()` pattern as `$lib/api/capacities.ts`.
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

/** One row of the loaded-set projection — model + state + the live AND-gate. */
export type ModelSetView = components['schemas']['ModelSetView'];
/** An advertised model on a runner's interface catalog (base or LoRA). */
export type ModelEntry = components['schemas']['ModelEntry'];
/** The operator-curated lifecycle position of a model in the pool. */
export type ModelState = components['schemas']['ModelState'];
/** Request body for the operator state-machine step. */
export type TransitionRequest = components['schemas']['TransitionRequest'];

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

// ── Model-pool endpoints ─────────────────────────────────────────────────────

/**
 * GET /api/v1/models — the loaded-set projection. Every model the operator has
 * curated into the pool, each decorated with its lifecycle `state` and the
 * `available` AND-gate (the flag the editor model picker filters on).
 */
export async function listLoadedModels(): Promise<ModelSetView[]> {
	return unwrap(await client.GET('/api/v1/models', {}));
}

/**
 * POST /api/v1/models/{model_id}/transition — the operator state-machine step.
 * `target` is validated against `ModelState::legal_transitions`; an illegal edge
 * returns 409 (surfaced as a thrown `Error` by `unwrap`).
 */
export async function transitionModel(
	modelId: string,
	target: ModelState,
	note?: string
): Promise<ModelSetView> {
	return unwrap(
		await client.POST('/api/v1/models/{model_id}/transition', {
			params: { path: { model_id: modelId } },
			body: { target, note: note ?? null }
		})
	);
}
