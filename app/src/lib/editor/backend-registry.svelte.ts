/**
 * Frontend cache of `GET /api/v1/backends`. Pairs with the Rust
 * `crate::backends::BACKENDS` registry — the API returns per-backend
 * metadata the editor needs (display label, icon, default config,
 * default output port, dispatch mode, resource channel, schedulability).
 *
 * Phase 1: only SMTP is in the registry; other backends still come from
 * the hardcoded ladders in `AutomatedStepSection.svelte` +
 * `automated-ports.ts`. As Phase 2 ports each backend, callers swap from
 * the legacy ladder to `getBackend(name)`.
 *
 * Caching: fetched once on first use, kept in module-scoped `$state`.
 * The data is workspace-agnostic and changes only on a backend redeploy,
 * so the session-lifetime cache is correct.
 */

import type { components } from '$lib/api/schema';

export type BackendDescriptor = components['schemas']['BackendDescriptor'];
export type ExecutionBackendType = components['schemas']['ExecutionBackendType'];

type RegistryState =
	| { kind: 'idle' }
	| { kind: 'loading'; promise: Promise<BackendDescriptor[]> }
	| { kind: 'ready'; backends: BackendDescriptor[] }
	| { kind: 'error'; message: string };

let state: RegistryState = $state({ kind: 'idle' });

async function fetchBackends(): Promise<BackendDescriptor[]> {
	const res = await fetch('/api/v1/backends', { credentials: 'same-origin' });
	if (!res.ok) {
		throw new Error(`GET /api/v1/backends failed: ${res.status}`);
	}
	return res.json() as Promise<BackendDescriptor[]>;
}

/**
 * Lazily fetch the registry. Subsequent calls return the cached result.
 * Callers that need a synchronous value should use
 * [`getCachedBackend`] AFTER an initial `loadBackends()` somewhere
 * higher in the tree (typically a `+layout.svelte` `onMount`).
 */
export async function loadBackends(): Promise<BackendDescriptor[]> {
	if (state.kind === 'ready') return state.backends;
	if (state.kind === 'loading') return state.promise;
	const promise = fetchBackends();
	state = { kind: 'loading', promise };
	try {
		const backends = await promise;
		state = { kind: 'ready', backends };
		return backends;
	} catch (err) {
		const message = err instanceof Error ? err.message : String(err);
		state = { kind: 'error', message };
		throw err;
	}
}

/**
 * Synchronous lookup that returns the cached descriptor or `undefined`.
 * Use only after `loadBackends()` has resolved — typically in components
 * that mount after a `+layout` load.
 */
export function getCachedBackend(name: ExecutionBackendType): BackendDescriptor | undefined {
	if (state.kind !== 'ready') return undefined;
	return state.backends.find((b) => b.name === name);
}

/**
 * Reactive list of backends. Returns `[]` until the first fetch resolves.
 * Use in Svelte components with `$derived` or template loops:
 *
 * ```svelte
 * <script>
 *   import { backendList, loadBackends } from '$lib/editor/backend-registry.svelte';
 *   onMount(() => { loadBackends(); });
 * </script>
 * {#each backendList() as b}
 *   <Select.Item value={b.name} label={b.displayName} />
 * {/each}
 * ```
 */
export function backendList(): BackendDescriptor[] {
	return state.kind === 'ready' ? state.backends : [];
}

/**
 * Whether the registry is loaded. Useful for `{#if backendsReady()}…{/if}`
 * gates that hide UI until metadata is available.
 */
export function backendsReady(): boolean {
	return state.kind === 'ready';
}

export type Port = components['schemas']['Port'];

/**
 * Server-side derive of the canonical output [`Port`] for a `Derived`
 * backend (LLM today). Calls `POST /api/v1/backends/{name}/derive-output`
 * with the step's current config; the server runs the backend's pure
 * `derive_output_port` and returns the resulting Port.
 *
 * Server-side keeps drift impossible: the same logic that the compiler
 * uses for publish-time validation also drives the editor preview, so a
 * config that lints clean in the editor will publish clean.
 *
 * Throws on network failure or non-200 response — callers should fall
 * back to the descriptor's `defaultOutputPort` on error.
 */
export async function deriveBackendOutput(
	name: ExecutionBackendType,
	config: unknown
): Promise<Port> {
	const res = await fetch(`/api/v1/backends/${name}/derive-output`, {
		method: 'POST',
		credentials: 'same-origin',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify(config ?? {})
	});
	if (!res.ok) {
		throw new Error(`POST /api/v1/backends/${name}/derive-output failed: ${res.status}`);
	}
	return res.json() as Promise<Port>;
}
