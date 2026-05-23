/**
 * Pure graph-layout for the Petri canvas.
 *
 * Owns the node-dimension constants (the single source of truth — LabCanvas
 * previously carried a copy with a "must match TransitionNode.svelte" comment)
 * and the dagre compound-layout + recomputed group-bounds pass. No DOM, no
 * Svelte reactivity — just `Node[]/Edge[]` in, positioned `Node[]/Edge[]` out.
 */

import dagre from '@dagrejs/dagre';
import { Position, type Node, type Edge } from '@xyflow/svelte';
import type { ScenarioGroup } from '$lib/types/petri';

// ── Node dimensions (single source of truth) ────────────────────────────────
export const PLACE_WIDTH = 70;
export const PLACE_HEIGHT = 70;
export const TRANSITION_WIDTH = 200;
export const GROUP_MIN_WIDTH = 180;
export const GROUP_MIN_HEIGHT = 100;

// Transition height is derived from port counts and must match the row
// metrics in TransitionNode.svelte. If those CSS/Tailwind values change,
// update these in lockstep — otherwise dagre packs the rows tighter than
// the rendered chip and adjacent nodes overlap.
// Worst-case header: a guard/effect badge ("G", "FX") is `px-1.5 py-0.5
// text-sm` → 20px line + 4px padding = 24px tall, so the row clears
// max(text, badge) + py-1 = 24 + 8 = 32. Plain (label-only) transitions
// over-pad by 4px — acceptable trade for keeping ports from overlapping.
const TRANS_HEADER_H = 32;
const TRANS_PORTS_PAD = 9; // border-t (1px) + py-1 (4px*2)
const TRANS_PORT_ROW = 22; // .port-row { min-height: 22px } in TransitionNode.svelte

export function getTransitionHeight(
	inputCount: number,
	outputCount: number,
	causedCount: number
): number {
	const rightRows = outputCount + (causedCount > 0 ? causedCount + 1 : 0); // +1 divider row
	const maxRows = Math.max(inputCount, rightRows, 1);
	return TRANS_HEADER_H + TRANS_PORTS_PAD + maxRows * TRANS_PORT_ROW;
}

// Meta-group node dimensions (must match MetaGroupNode.svelte).
// Header is two stacked text-sm rows (title + summary) inside `px-3 py-1.5`,
// so 2 × 20px line-height + 2 × 6px padding = 52. Ports section adds a 1px
// top border plus `py-1` (8px) above the first row.
export const META_WIDTH = 220;
const META_HEADER_H = 52;
const META_PORT_ROW = 16; // .port-row { height: 16px } in MetaGroupNode.svelte
const META_PAD = 9; // border-t (1px) + py-1 (4px*2)

export function getMetaHeight(inputCount: number, outputCount: number): number {
	const maxPorts = Math.max(inputCount, outputCount, 1);
	return META_HEADER_H + META_PAD + maxPorts * META_PORT_ROW;
}

/** Fallback dimensions by node type (non-transition nodes). */
export function getNodeDimensions(type: string): { width: number; height: number } {
	switch (type) {
		case 'place':
			return { width: PLACE_WIDTH, height: PLACE_HEIGHT };
		case 'transition':
			return { width: TRANSITION_WIDTH, height: getTransitionHeight(1, 1, 0) };
		case 'group':
			return { width: GROUP_MIN_WIDTH, height: GROUP_MIN_HEIGHT };
		case 'metagroup':
			return { width: META_WIDTH, height: getMetaHeight(1, 1) };
		case 'remotenet':
			return { width: 140, height: 56 };
		default:
			return { width: 100, height: 100 };
	}
}

/**
 * Layout nodes and edges with dagre (compound graph), then recompute group
 * bounds from actual child positions (dagre's compound sizing is unreliable).
 */
export function getLayoutedElements(
	nodes: Node[],
	edges: Edge[],
	groupNodes: ScenarioGroup[],
	spotlightActive: boolean = false
): { nodes: Node[]; edges: Edge[] } {
	// Always create a compound graph — nodes may have parentId even with no
	// explicit group.
	const dagreGraph = new dagre.graphlib.Graph({ compound: true });
	dagreGraph.setDefaultEdgeLabel(() => ({}));
	dagreGraph.setGraph({ rankdir: 'LR', nodesep: 4, ranksep: 12 });

	// Compute nesting depth for each group.
	const groupDepthMap = new Map<string, number>();
	const computeDepth = (group: ScenarioGroup): number => {
		if (groupDepthMap.has(group.id)) return groupDepthMap.get(group.id)!;
		const depth = group.parent_id
			? computeDepth(groupNodes.find((g) => g.id === group.parent_id)!) + 1
			: 0;
		groupDepthMap.set(group.id, depth);
		return depth;
	};
	groupNodes.forEach(computeDepth);

	// Group padding constants.
	const GROUP_PAD_LEFT = 12;
	const GROUP_PAD_RIGHT = 12;
	const GROUP_PAD_TOP = 22;
	const GROUP_PAD_BOTTOM = 8;

	// Add group nodes with padding so children don't crowd edges.
	groupNodes.forEach((group) => {
		dagreGraph.setNode(group.id, {
			width: GROUP_MIN_WIDTH,
			height: GROUP_MIN_HEIGHT,
			paddingLeft: GROUP_PAD_LEFT,
			paddingRight: GROUP_PAD_RIGHT,
			paddingTop: GROUP_PAD_TOP,
			paddingBottom: GROUP_PAD_BOTTOM
		});
		if (group.parent_id) {
			dagreGraph.setParent(group.id, group.parent_id);
		}
	});

	// Add regular nodes with their dimensions.
	nodes.forEach((node) => {
		const dims = (node.data as any)?._dims ?? getNodeDimensions(node.type ?? 'place');
		dagreGraph.setNode(node.id, dims);
		if (node.parentId) {
			dagreGraph.setParent(node.id, node.parentId);
		}
	});

	edges.forEach((edge) => {
		dagreGraph.setEdge(edge.source, edge.target);
	});

	dagre.layout(dagreGraph);

	// Recompute group bounds from actual child positions. Process leaf groups
	// first, then parents.
	const groupsByDepth = [...groupNodes].sort(
		(a, b) => (groupDepthMap.get(b.id) ?? 0) - (groupDepthMap.get(a.id) ?? 0)
	);

	// Absolute top-left + dimensions for every node (children + subgroups).
	const absRect = new Map<string, { x: number; y: number; w: number; h: number }>();

	nodes.forEach((node) => {
		const pos = dagreGraph.node(node.id);
		if (!pos) return;
		const dims = (node.data as any)?._dims ?? getNodeDimensions(node.type ?? 'place');
		absRect.set(node.id, {
			x: pos.x - dims.width / 2,
			y: pos.y - dims.height / 2,
			w: dims.width,
			h: dims.height
		});
	});

	// Build child-of-group map.
	const childrenOf = new Map<string, string[]>();
	groupNodes.forEach((g) => childrenOf.set(g.id, []));
	nodes.forEach((node) => {
		if (node.parentId && childrenOf.has(node.parentId)) {
			childrenOf.get(node.parentId)!.push(node.id);
		}
	});
	// Nested groups are also children of their parent group.
	groupNodes.forEach((g) => {
		if (g.parent_id && childrenOf.has(g.parent_id)) {
			childrenOf.get(g.parent_id)!.push(g.id);
		}
	});

	// Compute group bounds bottom-up (deepest first).
	const groupPositions = new Map<
		string,
		{ x: number; y: number; width: number; height: number }
	>();
	groupsByDepth.forEach((group) => {
		const kids = childrenOf.get(group.id) ?? [];
		if (kids.length === 0) {
			const pos = dagreGraph.node(group.id);
			if (pos) {
				const w = pos.width ?? GROUP_MIN_WIDTH;
				const h = pos.height ?? GROUP_MIN_HEIGHT;
				groupPositions.set(group.id, {
					x: pos.x - w / 2,
					y: pos.y - h / 2,
					width: w,
					height: h
				});
				absRect.set(group.id, { x: pos.x - w / 2, y: pos.y - h / 2, w, h });
			}
			return;
		}

		let minX = Infinity,
			minY = Infinity,
			maxX = -Infinity,
			maxY = -Infinity;
		kids.forEach((kid) => {
			const r = absRect.get(kid);
			if (!r) return;
			minX = Math.min(minX, r.x);
			minY = Math.min(minY, r.y);
			maxX = Math.max(maxX, r.x + r.w);
			maxY = Math.max(maxY, r.y + r.h);
		});

		if (!isFinite(minX)) {
			const pos = dagreGraph.node(group.id);
			if (pos) {
				const w = pos.width ?? GROUP_MIN_WIDTH;
				const h = pos.height ?? GROUP_MIN_HEIGHT;
				groupPositions.set(group.id, {
					x: pos.x - w / 2,
					y: pos.y - h / 2,
					width: w,
					height: h
				});
				absRect.set(group.id, { x: pos.x - w / 2, y: pos.y - h / 2, w, h });
			}
			return;
		}

		const gx = minX - GROUP_PAD_LEFT;
		const gy = minY - GROUP_PAD_TOP;
		const gw = Math.max(GROUP_MIN_WIDTH, maxX - minX + GROUP_PAD_LEFT + GROUP_PAD_RIGHT);
		const gh = Math.max(GROUP_MIN_HEIGHT, maxY - minY + GROUP_PAD_TOP + GROUP_PAD_BOTTOM);

		groupPositions.set(group.id, { x: gx, y: gy, width: gw, height: gh });
		absRect.set(group.id, { x: gx, y: gy, w: gw, h: gh });
	});

	// Build complete node list (groups first, then regular nodes).
	const allNodes: Node[] = [];

	groupNodes.forEach((group) => {
		const gp = groupPositions.get(group.id);
		if (!gp) return;

		let x = gp.x;
		let y = gp.y;

		if (group.parent_id) {
			const parentPos = groupPositions.get(group.parent_id);
			if (parentPos) {
				x -= parentPos.x;
				y -= parentPos.y;
			}
		}

		allNodes.push({
			id: group.id,
			type: 'group',
			position: { x, y },
			width: gp.width,
			height: gp.height,
			data: {
				label: group.name,
				depth: groupDepthMap.get(group.id) ?? 0,
				metadata: group.metadata,
				spotlightDimmed: spotlightActive
			},
			parentId: group.parent_id ?? undefined,
			extent: group.parent_id ? 'parent' : undefined,
			style: `width: ${gp.width}px; height: ${gp.height}px;`,
			zIndex: -1
		});
	});

	nodes.forEach((node) => {
		const rect = absRect.get(node.id);
		let x = rect ? rect.x : 0;
		let y = rect ? rect.y : 0;

		if (node.parentId) {
			const parentPos = groupPositions.get(node.parentId);
			if (parentPos) {
				x -= parentPos.x;
				y -= parentPos.y;
			}
		}

		allNodes.push({
			...node,
			targetPosition: Position.Left,
			sourcePosition: Position.Right,
			position: { x, y },
			extent: node.parentId ? 'parent' : undefined
		});
	});

	return { nodes: allNodes, edges };
}
