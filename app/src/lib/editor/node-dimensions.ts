/**
 * Single source of truth for workflow-editor node dimensions.
 *
 * Two consumers must agree on a node's footprint or graphs overlap:
 *   1. The rendered card — every leaf node passes `NODE_WIDTH[kind]` to
 *      `WorkflowNodeCard` as a fixed inline width, so a node's width is a
 *      function of its TYPE only (long titles / port names / channel names
 *      truncate inside the set width instead of widening the card).
 *   2. The auto-layout (`workflow-layout.ts`) and the demo position
 *      regeneration — both reserve `getWorkflowNodeDimensions(node)` per node
 *      so dagre never packs neighbours into a node's actual rendered box.
 *
 * Width is fixed per type. Height is DERIVED from content (declared output
 * fields, streaming channels, decision branches, …) because that's what
 * legitimately grows a card — the rows stack vertically. The height estimate
 * is row-metric based, calibrated to slightly OVER-reserve: that's the safe
 * direction (a little extra air between rows beats overlapping cards).
 *
 * This file is intentionally Svelte-free (type-only imports, erased at
 * runtime) so the headless demo-regen script can import it directly.
 */

import type { NodeKind, WorkflowNodeData } from '$lib/types/editor';

// ── Fixed width per node type ───────────────────────────────────────────────
// Comfortable floors that fit each card's typical content. Bump a value here
// AND nothing else — the card reads it via `NODE_WIDTH[kind]` and the layout
// reads it via `getWorkflowNodeDimensions`, so the two can't drift.
export const NODE_WIDTH: Record<NodeKind, number> = {
	start: 220,
	end: 220,
	human_task: 200,
	automated_step: 240,
	decision: 240,
	parallel_split: 170,
	join: 170,
	loop: 260,
	map: 260,
	scope: 200,
	lease_scope: 260,
	timeout: 260,
	phase_update: 190,
	progress_update: 190,
	failure: 190,
	delay: 190,
	trigger: 180,
	sub_workflow: 240,
	agent: 260
};

// Start / End collapse to a compact pill when they declare no fields — keep
// that pill narrow rather than forcing it to the full field-card width.
export const PILL_WIDTH = 120;

// Container kinds are resizable and own their stored width/height. These
// minimums MUST match the `NodeResizer minWidth/minHeight` in each container
// component (LoopNode/MapNode/ScopeNode/LeaseScopeNode/TimeoutNode).
export const CONTAINER_MIN: Record<string, { width: number; height: number }> = {
	loop: { width: 220, height: 140 },
	map: { width: 220, height: 140 },
	timeout: { width: 220, height: 140 },
	scope: { width: 200, height: 120 },
	lease_scope: { width: 240, height: 150 }
};

export function isContainerKind(type: string | undefined): boolean {
	return (
		type === 'scope' ||
		type === 'lease_scope' ||
		type === 'loop' ||
		type === 'timeout' ||
		type === 'map'
	);
}

// ── Height row metrics (must track WorkflowNodeCard + the node bodies) ───────
// `px-3 py-2` header with a `size-6` (24px) icon → 24 + 2×8 = 40.
const HEADER_H = 40;
// Body is `px-3 py-2` → 8 top + 8 bottom.
const BODY_PAD = 16;
// One `text-sm` line (label row, single-line body, section heading).
const LINE = 20;
// One declared field / mapping row (`space-y-0.5` list item).
const FIELD_ROW = 20;
// One streaming-channel row (a `py-0.5` badge inside `space-y-1`).
const CHANNEL_ROW = 26;
// One decision branch row (`h-6` = 24 + `gap-1` = 4).
const BRANCH_ROW = 28;
// Gap before a `border-t pt-1.5` sub-section (border + padding).
const SECTION_GAP = 8;
// Field-less Start/End pill (single `rounded-full` line).
const PILL_H = 36;

/**
 * Predict a card's rendered height from its content. Generous by design — see
 * the file header. Containers are handled by the layout (bounds come from
 * their children), so here they return their stored/min height as a fallback.
 */
export function getWorkflowNodeHeight(node: {
	type: string;
	data: WorkflowNodeData;
	height?: number | null;
}): number {
	const data = node.data;

	if (isContainerKind(node.type)) {
		const min = CONTAINER_MIN[node.type] ?? { width: 200, height: 120 };
		return node.height ?? min.height;
	}

	switch (data.type) {
		case 'start': {
			const n = data.initial?.fields?.length ?? 0;
			return n === 0 ? PILL_H : HEADER_H + BODY_PAD + LINE + n * FIELD_ROW;
		}
		case 'end': {
			const n = data.resultMapping?.length ?? 0;
			return n === 0 ? PILL_H : HEADER_H + BODY_PAD + LINE + n * FIELD_ROW;
		}
		case 'human_task': {
			const steps = data.steps?.length ?? 0;
			const fields =
				data.steps?.reduce(
					(sum, s) => sum + s.blocks.filter((b) => b.type === 'input').length,
					0
				) ?? 0;
			const secondLine = steps > 0 || fields > 0 ? LINE : 0;
			return HEADER_H + BODY_PAD + LINE + secondLine;
		}
		case 'automated_step': {
			let h = HEADER_H + BODY_PAD + LINE; // backend / deploy-chip row
			const fields = data.output?.fields?.length ?? 0;
			if (fields > 0) h += SECTION_GAP + LINE + fields * FIELD_ROW;
			const channels = data.channels?.length ?? 0;
			if (channels > 0) h += SECTION_GAP + LINE + channels * CHANNEL_ROW;
			return h;
		}
		case 'decision': {
			const branches =
				(data.conditions?.length ?? 0) + (data.defaultBranch ? 1 : 0);
			return HEADER_H + BODY_PAD + (branches > 0 ? branches * BRANCH_ROW : LINE);
		}
		case 'parallel_split':
			return HEADER_H; // no body
		case 'join':
		case 'trigger':
		case 'delay':
		case 'phase_update':
		case 'progress_update':
		case 'failure':
			return HEADER_H + BODY_PAD + LINE; // single-line body
		case 'sub_workflow': {
			let h = HEADER_H + BODY_PAD + LINE; // child / version row
			const inputs = data.inputContract?.fields?.length ?? 0;
			if (inputs > 0) h += SECTION_GAP + LINE + inputs * FIELD_ROW;
			const outputs = data.output?.fields?.length ?? 0;
			if (outputs > 0) h += SECTION_GAP + LINE + outputs * FIELD_ROW;
			return h;
		}
		case 'agent': {
			let h = HEADER_H + BODY_PAD + LINE; // provider / turns row
			if (data.model?.model) h += LINE; // model-name line
			const loopPath = (data.maxTurns ?? 1) > 1 || data.stopWhen != null;
			if (loopPath) h += SECTION_GAP + LINE; // "on tool error" row
			// Derived output port: canonical 4 LLM fields, +4 on the loop path.
			const fields = 4 + (loopPath ? 4 : 0);
			h += SECTION_GAP + LINE + fields * FIELD_ROW;
			return h;
		}
		default:
			return HEADER_H + BODY_PAD + LINE;
	}
}

/** Fixed width for a node, honouring the Start/End pill collapse. */
export function getWorkflowNodeWidth(node: {
	type: string;
	data: WorkflowNodeData;
	width?: number | null;
}): number {
	if (isContainerKind(node.type)) {
		const min = CONTAINER_MIN[node.type] ?? { width: 200, height: 120 };
		return node.width ?? min.width;
	}
	if (node.data.type === 'start' && (node.data.initial?.fields?.length ?? 0) === 0)
		return PILL_WIDTH;
	if (node.data.type === 'end' && (node.data.resultMapping?.length ?? 0) === 0)
		return PILL_WIDTH;
	return NODE_WIDTH[node.type as NodeKind] ?? 200;
}

export function getWorkflowNodeDimensions(node: {
	type: string;
	data: WorkflowNodeData;
	width?: number | null;
	height?: number | null;
}): { width: number; height: number } {
	return {
		width: getWorkflowNodeWidth(node),
		height: getWorkflowNodeHeight(node)
	};
}

/** Inline width a leaf card should render at, or `null` to stay intrinsic
 *  (field-less Start/End pills keep their natural size). */
export function cardWidthFor(node: { type: NodeKind; data: WorkflowNodeData }): number | null {
	if (node.data.type === 'start' && (node.data.initial?.fields?.length ?? 0) === 0) return null;
	if (node.data.type === 'end' && (node.data.resultMapping?.length ?? 0) === 0) return null;
	return NODE_WIDTH[node.type];
}
