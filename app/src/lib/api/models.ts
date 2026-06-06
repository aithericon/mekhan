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
/** Request body for operator curation — add a model to the workspace SET. */
export type CreateModelRequest = components['schemas']['CreateModelRequest'];
/** Request body for the runner-targeted load/unload (carries `runner_id`). */
export type LoadModelRequest = components['schemas']['LoadModelRequest'];
/** One live presence row from `GET /api/v1/runners/presence`. */
export type RunnerPresenceSnapshot = components['schemas']['RunnerPresenceSnapshot'];

/** Per-node engine inventory (docs/31 Phase 0) — `GET /api/v1/fleet/engines`. */
export type FleetEnginesResponse = components['schemas']['FleetEnginesResponse'];
/** One node's engines in the inventory. */
export type NodeInventory = components['schemas']['NodeInventory'];
/** One base engine on a node: base id, C, headroom, loaded adapters. */
export type NodeEngine = components['schemas']['NodeEngine'];
/** Per-policy model-replica autoscaler row (docs/29 §6'). */
export type ModelReplicaRow = components['schemas']['ModelReplicaRow'];
/** Per-pool node-replica autoscaler row (docs/31 Loop 1). */
export type NodeReplicaRow = components['schemas']['NodeReplicaRow'];
/** The load/unload/pull command wire envelope. */
export type ModelCommand = components['schemas']['ModelCommand'];

/** One model in an upstream catalog browse result (Ollama library / HF). */
export type CatalogModel = components['schemas']['CatalogModel'];
/** `GET /api/v1/model-catalog/{source}` response. */
export type ModelCatalogResponse = components['schemas']['ModelCatalogResponse'];
/** A model-browser catalog source. */
export type CatalogSource = 'ollama' | 'huggingface';

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

/**
 * Extract a human-readable message from an unknown thrown value. `unwrap` throws
 * `Error("API error <status>: <body>")` where `<body>` is the JSON-stringified
 * error payload; this peels that back to the server's `error`/`message` field so
 * callers can surface a clean toast instead of the raw envelope.
 */
export function apiErrorMessage(err: unknown): string {
	if (err instanceof Error) {
		const m = err.message.match(/^API error \d+: (.*)$/s);
		if (m) {
			const tail = m[1];
			try {
				const parsed = JSON.parse(tail) as { error?: unknown; message?: unknown };
				const field = parsed.error ?? parsed.message;
				if (typeof field === 'string') return field;
			} catch {
				// tail wasn't JSON — fall through to the raw tail
			}
			return tail;
		}
		return err.message;
	}
	return String(err);
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

/**
 * POST /api/v1/models — operator curation: add a model to the workspace SET. The
 * row lands in `approved` with zero replicas. 409 on the `(workspace, model_id)`
 * PK conflict (surfaced as a thrown `Error`). Returns the projected view.
 */
export async function createModel(body: CreateModelRequest): Promise<ModelSetView> {
	return unwrap(await client.POST('/api/v1/models', { body }));
}

/**
 * DELETE /api/v1/models/{model_id} — hard-delete a curated model row. `204` on
 * success; 404 when no row was removed (thrown by `unwrap`).
 */
export async function deleteModel(modelId: string): Promise<void> {
	const r = await client.DELETE('/api/v1/models/{model_id}', {
		params: { path: { model_id: modelId } }
	});
	if (r.error !== undefined) {
		throw new Error(`API error ${r.response.status}: ${JSON.stringify(r.error)}`);
	}
}

/**
 * POST /api/v1/models/{model_id}/load — operator load against a SPECIFIC runner.
 * UPSERTs the lifecycle row to `loading` then publishes a `Load{Base}` command to
 * the runner's model agent (fire-and-forget). Returns the projected view.
 */
export async function loadModel(modelId: string, runnerId: string): Promise<ModelSetView> {
	return unwrap(
		await client.POST('/api/v1/models/{model_id}/load', {
			params: { path: { model_id: modelId } },
			body: { runner_id: runnerId }
		})
	);
}

/**
 * POST /api/v1/models/{model_id}/unload — operator unload against a SPECIFIC
 * runner. Moves a `loaded`/`loading` row to `draining` and ALWAYS publishes an
 * `Unload{Base}` command to the runner. Returns the projected view.
 */
export async function unloadModel(modelId: string, runnerId: string): Promise<ModelSetView> {
	return unwrap(
		await client.POST('/api/v1/models/{model_id}/unload', {
			params: { path: { model_id: modelId } },
			body: { runner_id: runnerId }
		})
	);
}

/**
 * GET /api/v1/runners/presence — the live in-memory presence snapshot (the actual
 * pool-capacity signal). One row per runner in the caller's workspace, carrying
 * its advertised `backends` and whether it is currently `present`.
 */
export async function listRunnerPresence(): Promise<RunnerPresenceSnapshot[]> {
	return unwrap(await client.GET('/api/v1/runners/presence', {}));
}

/** GET /api/v1/fleet/engines — the live per-node engine inventory (Phase 0). */
export async function listFleetEngines(): Promise<FleetEnginesResponse> {
	return unwrap(await client.GET('/api/v1/fleet/engines', {}));
}

/** GET /api/v1/models/replicas — per-policy model-replica autoscaler rows. */
export async function listModelReplicas(): Promise<ModelReplicaRow[]> {
	return unwrap(await client.GET('/api/v1/models/replicas', {}));
}

/** GET /api/v1/node-replicas — per-pool node-replica autoscaler rows. */
export async function listNodeReplicas(): Promise<NodeReplicaRow[]> {
	return unwrap(await client.GET('/api/v1/node-replicas', {}));
}

/**
 * POST /api/v1/runners/{runner_id}/model-commands — place/evict a model on a
 * runner's local engine. `202` accepted (fire-and-forget; the agent applies it
 * and re-publishes its catalog, so the engine board reflects it on the next poll).
 */
export async function publishModelCommand(
	runnerId: string,
	cmd: ModelCommand
): Promise<void> {
	const r = await client.POST('/api/v1/runners/{runner_id}/model-commands', {
		params: { path: { runner_id: runnerId } },
		body: cmd
	});
	if (r.error !== undefined) {
		throw new Error(`API error ${r.response.status}: ${JSON.stringify(r.error)}`);
	}
}

/** Convenience: load/unload/pull a BASE model on a runner. */
export function baseCommand(verb: 'load' | 'unload' | 'pull', modelId: string): ModelCommand {
	return { kind: verb, target: { Base: { model_id: modelId } } } as ModelCommand;
}

/**
 * GET /api/v1/model-catalog/{source} — browse an upstream OFFICIAL catalog
 * (`ollama` scrapes ollama.com; `huggingface` calls the HF JSON API). Metadata
 * only; the result is cached server-side ~10 min. `q` is an optional free-text
 * search (empty ⇒ the upstream's popular/trending list).
 */
export async function listModelCatalog(
	source: CatalogSource,
	q?: string
): Promise<ModelCatalogResponse> {
	return unwrap(
		await client.GET('/api/v1/model-catalog/{source}', {
			params: { path: { source }, query: q ? { q } : {} }
		})
	);
}
