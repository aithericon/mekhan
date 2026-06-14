/**
 * Run context handed to the `artifactEmbed` page block (instance Report only).
 *
 * A free page / template Notes has no run, so it never gets a context — the
 * Insert affordance is hidden there and any stray embed node renders a neutral
 * placeholder (see ArtifactEmbedView).
 *
 * The block embeds a LIVE view of the run's renderable artifacts, so it must
 * reach the same `createProcessLiveStore` the Process Overview uses. To avoid
 * opening one SSE stream per embed block, the context MEMOIZES one store per
 * process — every block targeting the same process shares it. The owner (the
 * Report page) calls `destroy()` on unmount to tear all of them down.
 */
import { createProcessLiveStore } from '$lib/stores/process-live.svelte';

export interface EmbedProcess {
	id: string;
	name: string;
}

export interface ArtifactEmbedContext {
	/** Processes belonging to the host run — feeds the insert picker. Live. */
	readonly processes: EmbedProcess[];
	/**
	 * Shared, memoized live store for `processId` (one SSE per process across
	 * all embed blocks on the page). Created + `init()`-ed on first request.
	 */
	getArtifactStore(processId: string): ReturnType<typeof createProcessLiveStore>;
}

/**
 * Build an embed context backed by a memoized store map. `getProcesses` is read
 * live (a thunk) so the picker reflects processes that appear after mount. The
 * returned `destroy` tears down every store the page lazily created.
 */
export function createEmbedContext(getProcesses: () => EmbedProcess[]): {
	context: ArtifactEmbedContext;
	destroy: () => void;
} {
	const stores = new Map<string, ReturnType<typeof createProcessLiveStore>>();
	const context: ArtifactEmbedContext = {
		get processes() {
			return getProcesses();
		},
		getArtifactStore(processId: string) {
			let store = stores.get(processId);
			if (!store) {
				store = createProcessLiveStore(processId);
				store.init();
				stores.set(processId, store);
			}
			return store;
		}
	};
	return {
		context,
		destroy() {
			for (const store of stores.values()) store.destroy();
			stores.clear();
		}
	};
}
