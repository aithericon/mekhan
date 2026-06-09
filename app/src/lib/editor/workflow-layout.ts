/**
 * Dimension-aware auto-layout for the workflow editor graph.
 *
 * Every node is reserved at its real footprint (`node-dimensions.ts`) so dagre
 * never packs a neighbour onto a tall/wide card. Both the editor's
 * "Auto-arrange" button and `scripts/relayout-demos.mts` go through here.
 *
 * Layout is LEVELED, not dagre-compound. In this editor a container's
 * perimeter handles are real edge endpoints (`start → loop`, `loop → end`), so
 * sequence edges are incident on parent nodes — which dagre's compound ranker
 * cannot handle (it throws "Cannot set properties of undefined"). Instead we:
 *   1. lay out each container's direct children on their own flat canvas
 *      (innermost containers first) to discover the container's fitted size,
 *   2. lay out each level (top level + inside each container) flat, treating a
 *      container as one opaque box at its fitted size.
 * Cross-level edges (a container ↔ its own child) are ignored — only sibling
 * edges drive a level's ranking.
 *
 * Pure + Svelte-free (the only `$lib` import is type-only, erased at runtime)
 * so the headless demo-regen script can call it directly.
 *
 * Output positions follow the editor's storage convention: a child of a
 * container is positioned RELATIVE to its container's top-left; a top-level
 * node is absolute.
 */

import dagre from '@dagrejs/dagre';
import type { WorkflowNodeData } from '$lib/types/editor';
import {
	getWorkflowNodeDimensions,
	isContainerKind,
	CONTAINER_MIN
} from './node-dimensions';

export interface LayoutNode {
	id: string;
	type: string;
	data: WorkflowNodeData;
	parentId?: string | null;
	width?: number | null;
	height?: number | null;
	/**
	 * Real rendered footprint, if the node is currently mounted (xyflow writes
	 * `node.measured` after it measures the DOM). Preferred over the content
	 * estimate for leaf nodes so the layout reserves exactly what's painted —
	 * immune to estimate drift (CSS changes, live-fetched sub-workflow IO
	 * contracts the stored `data` snapshot hasn't caught up to, …). Absent in the
	 * headless demo-regen script, which falls back to the estimate.
	 */
	measuredWidth?: number | null;
	measuredHeight?: number | null;
}

export interface LayoutEdge {
	source: string;
	target: string;
	sourceHandle?: string | null;
	targetHandle?: string | null;
	type?: string | null;
	animated?: boolean;
}

export interface LayoutResult {
	/** New position per node id (parent-relative for parented children). */
	positions: Map<string, { x: number; y: number }>;
	/** New width/height per container node id (fit to its children). */
	containerSizes: Map<string, { width: number; height: number }>;
}

// Inner padding inside a container so children clear the dashed border and the
// title bar that sits along the top inside edge.
const PAD_LEFT = 16;
const PAD_RIGHT = 16;
const PAD_TOP = 32;
const PAD_BOTTOM = 16;

// Left-to-right flow matches how the editor reads (Start left, End right).
const RANK_DIR = 'LR';
const NODE_SEP = 36; // gap between same-rank (stacked) nodes
const RANK_SEP = 70; // gap between successive ranks (flow direction)

/**
 * An edge counts as "flow" (drives dagre ranking) unless it's a loop-back, a
 * tool binding, or a container body-wiring arc — those close cycles / connect a
 * container to its own children and would corrupt the left-to-right ranks.
 */
function isFlowEdge(e: LayoutEdge): boolean {
	if (e.type === 'loop_back' || e.type === 'tools') return false;
	if (e.animated) return false; // canvas marks loop-back/body-return edges animated
	if (e.sourceHandle === 'tools' || e.sourceHandle === 'body_in') return false;
	if (e.targetHandle === 'body_out') return false;
	return true;
}

const containerMinOf = (type: string) => CONTAINER_MIN[type] ?? { width: 200, height: 120 };

// A data-plane channel edge renders an inline live-media preview (~240×136)
// ON the edge in the instance view (see edge media feeds). Reserve that box as
// a dagre edge label so the rank gap widens to fit the preview instead of it
// overlapping the source/target cards. Control channels carry tokens, not
// bytes, so they get no preview and no reservation.
const STREAM_PREVIEW = { width: 256, height: 150 };

/**
 * Flat dagre over one set of sibling nodes. Returns each node's top-left,
 * normalised so the whole cluster starts at (0, 0). `edgeLabel` optionally
 * reserves a label box on an edge (used for streaming-media previews).
 */
function layoutFlat(
	members: LayoutNode[],
	edges: LayoutEdge[],
	sizeOf: (n: LayoutNode) => { width: number; height: number },
	edgeLabel?: (e: LayoutEdge) => { width: number; height: number } | null
): Map<string, { x: number; y: number }> {
	const tl = new Map<string, { x: number; y: number }>();
	if (members.length === 0) return tl;

	const g = new dagre.graphlib.Graph();
	g.setDefaultEdgeLabel(() => ({}));
	g.setGraph({ rankdir: RANK_DIR, nodesep: NODE_SEP, ranksep: RANK_SEP });

	const ids = new Set(members.map((m) => m.id));
	for (const m of members) {
		const d = sizeOf(m);
		g.setNode(m.id, { width: d.width, height: d.height });
	}
	for (const e of edges) {
		if (e.source === e.target) continue;
		if (!ids.has(e.source) || !ids.has(e.target)) continue;
		if (!isFlowEdge(e)) continue;
		const lbl = edgeLabel?.(e) ?? null;
		if (lbl) g.setEdge(e.source, e.target, { width: lbl.width, height: lbl.height, labelpos: 'c' });
		else g.setEdge(e.source, e.target);
	}

	dagre.layout(g);

	let minX = Infinity;
	let minY = Infinity;
	for (const m of members) {
		const p = g.node(m.id);
		const d = sizeOf(m);
		const x = (p?.x ?? 0) - d.width / 2;
		const y = (p?.y ?? 0) - d.height / 2;
		tl.set(m.id, { x, y });
		minX = Math.min(minX, x);
		minY = Math.min(minY, y);
	}
	if (!isFinite(minX)) return tl;
	for (const [id, p] of tl) tl.set(id, { x: p.x - minX, y: p.y - minY });
	return tl;
}

export function layoutWorkflowGraph(
	nodes: LayoutNode[],
	edges: LayoutEdge[]
): LayoutResult {
	const byId = new Map(nodes.map((n) => [n.id, n]));
	const hasContainerParent = (n: LayoutNode): boolean =>
		!!n.parentId && isContainerKind(byId.get(n.parentId)?.type);

	// An edge leaving a producer's data-plane channel handle gets an inline
	// live-preview in the instance view — reserve room for it.
	const edgeLabel = (e: LayoutEdge): { width: number; height: number } | null => {
		// eslint-disable-next-line @typescript-eslint/no-explicit-any
		const chans = (byId.get(e.source)?.data as any)?.channels;
		if (!Array.isArray(chans)) return null;
		const isData = chans.some(
			// eslint-disable-next-line @typescript-eslint/no-explicit-any
			(c: any) => c?.name === e.sourceHandle && c?.direction === 'out' && c?.plane === 'data'
		);
		return isData ? STREAM_PREVIEW : null;
	};

	// Direct children per container.
	const childrenOf = new Map<string, LayoutNode[]>();
	for (const n of nodes) {
		if (!hasContainerParent(n)) continue;
		const list = childrenOf.get(n.parentId!) ?? [];
		list.push(n);
		childrenOf.set(n.parentId!, list);
	}

	const positions = new Map<string, { x: number; y: number }>();
	const containerSizes = new Map<string, { width: number; height: number }>();

	const sizeOf = (n: LayoutNode): { width: number; height: number } => {
		if (isContainerKind(n.type)) {
			// A container's footprint is recomputed from its children (deepest-first
			// into `containerSizes`); its own `measured` is the stale pre-layout box,
			// so never trust it here.
			return (
				containerSizes.get(n.id) ?? {
					width: n.width ?? containerMinOf(n.type).width,
					height: n.height ?? containerMinOf(n.type).height
				}
			);
		}
		// Leaf nodes: prefer the real measured DOM box when mounted, else estimate.
		const est = getWorkflowNodeDimensions(n);
		return {
			width: n.measuredWidth && n.measuredWidth > 0 ? n.measuredWidth : est.width,
			height: n.measuredHeight && n.measuredHeight > 0 ? n.measuredHeight : est.height
		};
	};

	// Nesting depth so containers resolve deepest-first (a container's fitted
	// size must be known before its own parent level is laid out).
	const depthOf = (n: LayoutNode): number => {
		let d = 0;
		let pid = n.parentId;
		while (pid && byId.has(pid)) {
			d += 1;
			pid = byId.get(pid)!.parentId;
		}
		return d;
	};

	const containers = nodes
		.filter((n) => isContainerKind(n.type))
		.sort((a, b) => depthOf(b) - depthOf(a));

	for (const c of containers) {
		const kids = childrenOf.get(c.id) ?? [];
		const min = containerMinOf(c.type);
		if (kids.length === 0) {
			containerSizes.set(c.id, {
				width: c.width ?? min.width,
				height: c.height ?? min.height
			});
			continue;
		}

		const childTL = layoutFlat(kids, edges, sizeOf, edgeLabel);
		let contentW = 0;
		let contentH = 0;
		for (const kid of kids) {
			const tl = childTL.get(kid.id) ?? { x: 0, y: 0 };
			const d = sizeOf(kid);
			// Children sit at PAD offset inside the container.
			positions.set(kid.id, {
				x: Math.round(tl.x + PAD_LEFT),
				y: Math.round(tl.y + PAD_TOP)
			});
			contentW = Math.max(contentW, tl.x + d.width);
			contentH = Math.max(contentH, tl.y + d.height);
		}

		containerSizes.set(c.id, {
			width: Math.round(Math.max(min.width, contentW + PAD_LEFT + PAD_RIGHT)),
			height: Math.round(Math.max(min.height, contentH + PAD_TOP + PAD_BOTTOM))
		});
	}

	// Top level: nodes without a container parent. Containers among them are
	// opaque boxes at their fitted size.
	const topLevel = nodes.filter((n) => !hasContainerParent(n));
	const topTL = layoutFlat(topLevel, edges, sizeOf, edgeLabel);
	for (const n of topLevel) {
		const tl = topTL.get(n.id) ?? { x: 0, y: 0 };
		positions.set(n.id, { x: Math.round(tl.x), y: Math.round(tl.y) });
	}

	return { positions, containerSizes };
}
