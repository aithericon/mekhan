/**
 * Frontend cache of `GET /api/v1/node-library` — the "Library" half of the
 * editor palette (branded, reusable sub-workflow building blocks). Sister file
 * to `node-registry.svelte.ts` (the primitive node-type registry); same lazy
 * cache shape.
 *
 * Difference from node-types: this list is data-driven and ACL-filtered per
 * caller (it reflects which library nodes the caller's workspace can see), so
 * it is workspace-scoped rather than redeploy-scoped. We still cache it for the
 * editor session — switching workspaces reloads the editor anyway.
 *
 * Also owns the small "recently used" list (persisted to localStorage), keyed
 * by coordinate, surfaced as a Recent group at the top of the Library palette.
 */

import type { components } from '$lib/api/schema';

export type LibraryNodeDescriptor = components['schemas']['LibraryNodeDescriptor'];

type RegistryState =
	| { kind: 'idle' }
	| { kind: 'loading'; promise: Promise<LibraryNodeDescriptor[]> }
	| { kind: 'ready'; nodes: LibraryNodeDescriptor[] }
	| { kind: 'error'; message: string };

let state: RegistryState = $state({ kind: 'idle' });

async function fetchLibraryNodes(): Promise<LibraryNodeDescriptor[]> {
	const res = await fetch('/api/v1/node-library', { credentials: 'same-origin' });
	if (!res.ok) {
		throw new Error(`GET /api/v1/node-library failed: ${res.status}`);
	}
	return res.json() as Promise<LibraryNodeDescriptor[]>;
}

/**
 * Lazily fetch the library catalogue. Subsequent calls return the cached
 * result. Use [`libraryNodeList`] for a synchronous reactive read after the
 * first `loadLibraryNodes()` resolves.
 */
export async function loadLibraryNodes(): Promise<LibraryNodeDescriptor[]> {
	if (state.kind === 'ready') return state.nodes;
	if (state.kind === 'loading') return state.promise;
	const promise = fetchLibraryNodes();
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

/** Reactive list of library-node descriptors. `[]` until the first fetch. */
export function libraryNodeList(): LibraryNodeDescriptor[] {
	return state.kind === 'ready' ? state.nodes : [];
}

/** Whether the library catalogue is loaded. */
export function libraryNodesReady(): boolean {
	return state.kind === 'ready';
}

// --- Recently used (by coordinate) -----------------------------------------

const RECENT_KEY = 'mekhan:library:recent';
const RECENT_MAX = 6;

function loadRecent(): string[] {
	if (typeof localStorage === 'undefined') return [];
	try {
		const raw = localStorage.getItem(RECENT_KEY);
		if (!raw) return [];
		const parsed = JSON.parse(raw);
		return Array.isArray(parsed) ? parsed.filter((c) => typeof c === 'string') : [];
	} catch {
		return [];
	}
}

let recent: string[] = $state(loadRecent());

function persistRecent() {
	if (typeof localStorage === 'undefined') return;
	try {
		localStorage.setItem(RECENT_KEY, JSON.stringify(recent));
	} catch {
		/* quota / private mode — recent list is best-effort */
	}
}

/** Recently dropped library-node coordinates, most-recent first. */
export function recentCoordinates(): string[] {
	return recent;
}

/** Record a coordinate as most-recently used (de-duped, capped). */
export function markLibraryNodeUsed(coordinate: string) {
	recent = [coordinate, ...recent.filter((c) => c !== coordinate)].slice(0, RECENT_MAX);
	persistRecent();
}
