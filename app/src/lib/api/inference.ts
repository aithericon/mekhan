/**
 * Typed wrapper for the inference audit ledger read endpoint.
 *
 * `GET /api/v1/inference/requests` is the durable metering / GDPR-processing
 * ledger — one `InferenceRequestLogRow` per served inference request, newest
 * first. Inference bypasses the engine net (the HTTP router meters directly), so
 * this ledger is the only durable record of who served what, with which token
 * counts and outcome. The Control-Plane "Inference audit" surface reads it.
 *
 * Optional `instance_id` scopes to one workflow instance's requests; `limit`
 * caps the row count (server default 100, capped at 500).
 *
 * Same `openapi-fetch` client + `unwrap()` pattern as `$lib/api/models.ts`.
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

/** One inference metering / GDPR processing record from the audit ledger. */
export type InferenceRequestLogRow = components['schemas']['InferenceRequestLogRow'];

/** Point-in-time router operational gauges (proxied from the router /metrics). */
export type RouterLiveMetrics = components['schemas']['RouterLiveMetrics'];
export type RouterReplicaLive = components['schemas']['RouterReplicaLive'];
export type RouterModelLive = components['schemas']['RouterModelLive'];
/** One `(bucket, model)` rollup of the inference ledger over time. */
export type InferenceTimeseriesPoint = components['schemas']['InferenceTimeseriesPoint'];

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

// ── Inference audit endpoint ──────────────────────────────────────────────────

/**
 * GET /api/v1/inference/requests — the inference audit ledger, newest-first.
 * `instanceId` restricts to one workflow instance's requests; `limit` caps the
 * row count (server default 100, capped at 500).
 */
export async function listInferenceRequests(params?: {
	instanceId?: string;
	limit?: number;
}): Promise<InferenceRequestLogRow[]> {
	return unwrap(
		await client.GET('/api/v1/inference/requests', {
			params: {
				query: {
					instance_id: params?.instanceId ?? null,
					limit: params?.limit ?? null
				}
			}
		})
	);
}

// ── Live router gauges + historical timeseries (the "real data" telemetry) ───

/**
 * GET /api/v1/inference/router-live — point-in-time router operational gauges,
 * proxied + parsed from the router's `/metrics` exposition. Fail-soft: when the
 * router is unconfigured/unreachable the server returns `{ available: false }`
 * with a 200, so callers should branch on `available` rather than catch.
 */
export async function getRouterLive(): Promise<RouterLiveMetrics> {
	return unwrap(await client.GET('/api/v1/inference/router-live', {}));
}

/**
 * GET /api/v1/inference/timeseries — per-model throughput / latency / error
 * timeseries, time-bucketed over the durable inference ledger. `bucketSecs`
 * (5..3600, default 60) sets the bucket width; `windowSecs` (≤ 7d, default 3600)
 * the look-back. Optional `model` / `instanceId` filters. Oldest bucket first.
 */
export async function listInferenceTimeseries(params?: {
	bucketSecs?: number;
	windowSecs?: number;
	model?: string;
	instanceId?: string;
}): Promise<InferenceTimeseriesPoint[]> {
	return unwrap(
		await client.GET('/api/v1/inference/timeseries', {
			params: {
				query: {
					bucket_secs: params?.bucketSecs ?? null,
					window_secs: params?.windowSecs ?? null,
					model: params?.model ?? null,
					instance_id: params?.instanceId ?? null
				}
			}
		})
	);
}
