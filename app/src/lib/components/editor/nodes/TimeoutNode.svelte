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
	`parentId == timeout.id`. The compiler routes the inbound token in via
	`body_in` (source handle, interior-right edge) and back via `body_out`
	(target handle, interior-left edge). Two outer source handles: `out`
	(default; fires when the body wins the race) and `timeout` (fires when
	the timer wins).
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
	style="top: 35%;"
/>
<Handle
	id="timeout"
	type="source"
	position={Position.Right}
	class={workflowNodeHandleClass('timeout')}
	title="Timed out — body did not finish before deadline"
	style="top: 65%;"
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

	<!-- Inner-facing body handles (solid fill to distinguish from perimeter) -->
	<Handle
		id="body_in"
		type="source"
		position={Position.Left}
		class="!h-3 !w-3 !border-2 !border-node-timeout !bg-node-timeout"
		style="left: 16px; top: 50%;"
		title="Body in — start of watched work"
	/>
	<Handle
		id="body_out"
		type="target"
		position={Position.Right}
		class="!h-3 !w-3 !border-2 !border-node-timeout !bg-node-timeout"
		style="right: 16px; top: 50%;"
		title="Body out — body completion (cancels the timer)"
	/>
</div>
