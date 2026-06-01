/**
 * Typed wrappers for the multi-cluster management endpoints (docs/16 §8):
 * `GET /api/v1/clusters` (registered datacenters + live ClusterRegistry state)
 * and the per-cluster lifecycle actions `reconnect` / `drain`.
 *
 * Mirrors `$lib/api/resources.ts`: same `openapi-fetch` client, the
 * "throws on non-2xx" contract, and `components['schemas'][...]` aliases so call
 * sites never reach into `schema.d.ts` directly.
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

function unwrap<T>(result: {
	data?: T;
	error?: unknown;
	response: Response;
}): T {
	if (result.error !== undefined) {
		const body =
			typeof result.error === 'object' ? JSON.stringify(result.error) : String(result.error);
		throw new Error(`API error ${result.response.status}: ${body}`);
	}
	if (result.data === undefined) {
		throw new Error(`API error ${result.response.status}: empty body`);
	}
	return result.data;
}

// Type aliases

export type ClusterSummary = components['schemas']['ClusterSummary'];
export type ClusterActionResponse = components['schemas']['ClusterActionResponse'];

// Calls

/** Every REGISTERED datacenter, overlaid with live cluster state. Idle/un-leased
 *  datacenters still appear (`watcher_state: "idle"`); the lease-adapter pool-net
 *  keeps running regardless. */
export async function listClusters(): Promise<ClusterSummary[]> {
	const r = unwrap(await client.GET('/api/v1/clusters', {}));
	return r.clusters;
}

/** Force-reconnect: the engine drops the watcher + allocator session so the next
 *  fire rebuilds the client. No-op (`applied: false`) when no client is resident. */
export async function reconnectCluster(
	resourceId: string
): Promise<ClusterActionResponse> {
	return unwrap(
		await client.POST('/api/v1/clusters/{resource_id}/reconnect', {
			params: { path: { resource_id: resourceId } }
		})
	);
}

/** Drain: the cluster refuses new leases while held ones finish. */
export async function drainCluster(
	resourceId: string
): Promise<ClusterActionResponse> {
	return unwrap(
		await client.POST('/api/v1/clusters/{resource_id}/drain', {
			params: { path: { resource_id: resourceId } }
		})
	);
}
