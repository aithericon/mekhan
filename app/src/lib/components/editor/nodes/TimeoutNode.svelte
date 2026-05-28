<script lang="ts">
	import { getContext } from 'svelte';
	import { Handle, NodeResizer, Position } from '@xyflow/svelte';
	import type { TimeoutNodeData } from '$lib/types/editor';
	import TimerOff from '@lucide/svelte/icons/timer-off';
	import { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';
	import NodeRuntimeBadge from '$lib/components/instances/NodeRuntimeBadge.svelte';
	import {
		RESIZE_REPORT_CONTEXT_KEY,
		type ResizeReport
	} from './resize-context';

	let { id, data, selected }: { id: string; data: TimeoutNodeData; selected?: boolean } = $props();

	const reportResize = getContext<ResizeReport | undefined>(RESIZE_REPORT_CONTEXT_KEY);

	// Header reads as `<label> · within {expr} ms` — same compact pattern as
	// LoopNode's `while {cond}`.
	const EXPR_MAX = 24;
	let exprText = $derived.by(() => {
		const e = (data.durationMsExpr ?? '').trim();
		if (!e) return 'no deadline';
		return e.length > EXPR_MAX ? `within ${e.slice(0, EXPR_MAX - 1)}…` : `within ${e} ms`;
	});
</script>

<!--
	Timeout is a body-container that races a wrapped subgraph against a
	deadline. Body authoring places child nodes inside its bounds with
	`parentId == timeout.id` (drag-into / drag-out parenting is enabled in
	WorkflowCanvas's `isContainer` set).

	Port layout — three edges, three semantics:
	  • LEFT  (in, target)   — entry
	  • RIGHT (out, source)  — happy path: body finished before deadline
	  • BOTTOM (timeout, source, amber-tinted) — exception path: timer won
	The exception output lives on a different EDGE than the happy path so
	the two race outcomes are visually unambiguous, matching the "fallout"
	intuition (timeout drops down, not sideways).

	Body wiring (interior handles, solid-filled to distinguish from the
	outline-style outer perimeter handles):
	  • body_in  (interior left, source) — start of watched work
	  • body_out (interior right, target) — body completion (auto-cancels
	    the parent timer); arrives via `loop_back` edge type to mirror Loop.
-->
<NodeResizer
	isVisible={selected && !!reportResize}
	minWidth={220}
	minHeight={140}
	onResizeEnd={(_e, params) => reportResize?.(id, params)}
/>

<!-- Outer perimeter handles -->
<Handle
	id="in"
	type="target"
	position={Position.Left}
	class={workflowNodeHandleClass('timeout')}
	title="Flow in — enter timeout"
/>
<Handle
	id="out"
	type="source"
	position={Position.Right}
	class={workflowNodeHandleClass('timeout')}
	title="Done — body completed in time"
/>
<Handle
	id="timeout"
	type="source"
	position={Position.Bottom}
	class="!h-3 !w-3 !rounded-full !border-2 !border-amber-500 !bg-card"
	title="Timed out — body did not finish before deadline"
/>

<div
	class="relative h-full w-full rounded-xl border-2 border-dashed {selected
		? 'border-node-timeout bg-node-timeout/10'
		: 'border-node-timeout/50 bg-node-timeout/5'}"
	data-testid="node-timeout"
>
	<div class="absolute left-3 right-3 top-2 flex items-center gap-2 text-sm font-medium text-muted-foreground">
		<TimerOff class="size-4 text-node-timeout" />
		<span>{data.label}</span>
		<span class="font-mono text-sm">· {exprText}</span>
		<div class="ml-auto">
			<NodeRuntimeBadge nodeId={id} />
		</div>
	</div>

	<!--
		Inner-facing body handles (solid fill to distinguish from perimeter).
		Their `position` points INWARD toward the body interior where the
		wrapped children live: body_in uses Position.Right so its connection
		leaves rightward into the body, body_out uses Position.Left so the
		return arc enters from the body. The `left/right: auto` overrides keep
		them pinned to the inner walls (16px) without the position class's
		default wall-anchor fighting the styled placement.
	-->
	<Handle
		id="body_in"
		type="source"
		position={Position.Right}
		class="!h-3 !w-3 !border-2 !border-node-timeout !bg-node-timeout"
		style="left: 16px; right: auto; top: 50%;"
		title="Body in — start of watched work"
	/>
	<Handle
		id="body_out"
		type="target"
		position={Position.Left}
		class="!h-3 !w-3 !border-2 !border-node-timeout !bg-node-timeout"
		style="right: 16px; left: auto; top: 50%;"
		title="Body out — body completion (cancels the timer)"
	/>
</div>
