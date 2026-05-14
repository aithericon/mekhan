// Rune-backed store of the latest publish-attempt compile errors. The canvas
// reads it to add a red ring on offending node/edge ids; the editor route
// writes it from a caught `CompileApiError`. Cleared on the next successful
// publish (or when the user resets explicitly).

import type { CompileErrorView } from '$lib/api/client';

class CompileErrorStore {
	errors = $state<CompileErrorView[]>([]);
	// Indexed lookups so the canvas's per-node / per-edge query is O(1) per id.
	byNodeId = $derived(
		new Map(
			this.errors
				.filter((e) => e.node_id)
				.map((e) => [e.node_id as string, e] as const)
		)
	);
	byEdgeId = $derived(
		new Map(
			this.errors
				.filter((e) => e.edge_id)
				.map((e) => [e.edge_id as string, e] as const)
		)
	);

	set(errors: CompileErrorView[]) {
		this.errors = errors;
	}

	clear() {
		this.errors = [];
	}
}

export const compileErrors = new CompileErrorStore();
