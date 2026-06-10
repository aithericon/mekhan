/**
 * Edge join-discipline edits are reported up to `WorkflowCanvas.svelte`
 * through a Svelte context callback rather than per-edge props — the same
 * pattern as `nodes/resize-context.ts`: xyflow renders custom edges from a
 * registry (`edgeTypes`), so the canvas can't thread a callback prop through,
 * but it DOES own both the granular emit channel (`onUpdateEdge`) and the bulk
 * fallback (`serializeAndEmit`).
 *
 * The canvas registers a `SetEdgeJoin` under `EDGE_JOIN_CONTEXT_KEY` when not
 * readonly; `DeletableEdge.svelte` reads it for the control-channel join chip
 * and falls back to a display-only chip when absent (readonly canvases, the
 * instance/run view, isolated previews).
 */
import { getContext, setContext } from 'svelte';

/**
 * Set or clear an edge's channel join discipline. `'gather'` opts in to the
 * collect-into-one-array fold; `null` restores the implicit `'each'` default
 * (the stored key is deleted — normalize-to-default, see
 * `YjsGraphBinding.updateEdgeJoin`).
 */
export type SetEdgeJoin = (edgeId: string, join: 'gather' | null) => void;

export const EDGE_JOIN_CONTEXT_KEY = Symbol('edge-join');

export function provideEdgeJoin(setter: SetEdgeJoin): void {
	setContext(EDGE_JOIN_CONTEXT_KEY, setter);
}

/**
 * The join setter, or `undefined` when no provider is present (readonly /
 * instance view). Callers MUST treat `undefined` as "render display-only".
 */
export function useEdgeJoin(): SetEdgeJoin | undefined {
	return getContext<SetEdgeJoin | undefined>(EDGE_JOIN_CONTEXT_KEY);
}
