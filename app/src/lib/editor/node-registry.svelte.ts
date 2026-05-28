/**
 * Frontend cache of `GET /api/v1/node-types`. Pairs with the Rust
 * `crate::nodes::NODES` registry — the API returns per-variant metadata
 * (wire name, display label, description, runtime kind, protocol flags)
 * that the editor palette consumes so the variant list lives in ONE
 * place rather than being hand-mirrored on both sides.
 *
 * Sister file to `backend-registry.svelte.ts`; identical shape.
 *
 * What stays frontend-only:
 *  - Lucide icon imports + Tailwind colour classes (`node-palette-meta.ts`)
 *    — Svelte components can't be serialized through JSON.
 *  - The xyflow `nodeTypes` Svelte-component map.
 *  - Per-section property panel chain (each section is a Svelte component).
 *  - `createDefaultNodeData` — constructs typed `WorkflowNodeData` objects
 *    for every palette drag; a network round-trip per drop would lag the
 *    editor.
 *  - `derived-ports.ts` — runs per editor keystroke; same lag argument.
 *
 * Caching: fetched once on first use, kept in module-scoped `$state`. The
 * data is workspace-agnostic and changes only on a mekhan-service redeploy.
 */

import type { components } from '$lib/api/schema';

export type NodeDescriptor = components['schemas']['NodeDescriptor'];

type RegistryState =
	| { kind: 'idle' }
	| { kind: 'loading'; promise: Promise<NodeDescriptor[]> }
	| { kind: 'ready'; nodes: NodeDescriptor[] }
	| { kind: 'error'; message: string };

let state: RegistryState = $state({ kind: 'idle' });

async function fetchNodeTypes(): Promise<NodeDescriptor[]> {
	const res = await fetch('/api/v1/node-types', { credentials: 'same-origin' });
	if (!res.ok) {
		throw new Error(`GET /api/v1/node-types failed: ${res.status}`);
	}
	return res.json() as Promise<NodeDescriptor[]>;
}

/**
 * Lazily fetch the registry. Subsequent calls return the cached result.
 * Callers that need a synchronous value should use [`nodeList`] AFTER an
 * initial `loadNodeTypes()` somewhere higher in the tree (typically a
 * `+layout.svelte` `onMount`).
 */
export async function loadNodeTypes(): Promise<NodeDescriptor[]> {
	if (state.kind === 'ready') return state.nodes;
	if (state.kind === 'loading') return state.promise;
	const promise = fetchNodeTypes();
	state = { kind: 'loading', promise };
	try {
		const nodes = await promise;
		state = { kind: 'ready', nodes };
		return nodes;
	} catch (err) {
		const message = err instanceof Error ? err.message : String(err);
		state = { kind: 'error', message };
		throw err;
	}
}

/**
 * Reactive list of node descriptors. Returns `[]` until the first fetch
 * resolves.
 */
export function nodeList(): NodeDescriptor[] {
	return state.kind === 'ready' ? state.nodes : [];
}

/** Whether the registry is loaded. */
export function nodeTypesReady(): boolean {
	return state.kind === 'ready';
}
