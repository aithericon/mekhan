/**
 * Container-node resize is reported up to `WorkflowCanvas.svelte` through a
 * Svelte context callback rather than per-node props. xyflow's `NodeResizer`
 * fires `onResizeEnd` inside the node component, but the canvas owns the
 * granular emit channel (`onResizeNodes`) and the bulk fallback
 * (`serializeAndEmit`) — keeping the wiring in context avoids threading a
 * callback through every node type.
 *
 * The canvas registers a `ResizeReport` under `RESIZE_REPORT_CONTEXT_KEY`
 * when not readonly; container nodes (`LoopNode`, `ScopeNode`) read it and
 * fall back to a no-op when absent (readonly canvases, isolated previews,
 * or test renders without a canvas).
 */

/** Final gesture state from `NodeResizer.onResizeEnd`. Mirrors xyflow's
 *  `ResizeParams` so we don't depend on `@xyflow/system`'s deeper paths. */
export type ResizeParams = {
	x: number;
	y: number;
	width: number;
	height: number;
};

export type ResizeReport = (nodeId: string, params: ResizeParams) => void;

export const RESIZE_REPORT_CONTEXT_KEY = 'mekhan:reportResize';
