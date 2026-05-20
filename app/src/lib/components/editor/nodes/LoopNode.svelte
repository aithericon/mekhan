<script lang="ts">
	import { Handle, NodeResizer, Position } from '@xyflow/svelte';
	import type { LoopNodeData } from '$lib/types/editor';
	import Repeat from '@lucide/svelte/icons/repeat';
	import { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { data, selected }: { data: LoopNodeData; selected?: boolean } = $props();
</script>

<!--
	Loop is a container: body authoring places child nodes inside its bounds
	with `parentId == loop.id`. The compiler routes the iteration token in via
	`body_in` (source handle, interior-right edge of the container) and back
	out via `body_out` (target handle, interior-left edge). The outer
	`in`/`out` handles sit on the perimeter and connect to the parent flow.
-->
<NodeResizer
	isVisible={selected}
	minWidth={220}
	minHeight={140}
/>

<!-- Outer perimeter handles: connect the loop to the parent flow. -->
<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('loop')} />
<Handle id="out" type="source" position={Position.Right} class={workflowNodeHandleClass('loop')} />

<div
	class="relative h-full w-full rounded-xl border-2 border-dashed {selected
		? 'border-node-loop bg-node-loop/10'
		: 'border-node-loop/50 bg-node-loop/5'}"
	data-testid="node-loop"
>
	<!--
		Title bar — sits in the top inside edge so it doesn't eat the perimeter
		handle area. Shows the kind + label + iteration cap.
	-->
	<div class="absolute left-3 top-2 flex items-center gap-2 text-sm font-medium text-muted-foreground">
		<Repeat class="size-4 text-node-loop" />
		<span>{data.label}</span>
		<span class="text-xs">· max {data.maxIterations ?? 3}</span>
	</div>

	<!--
		Inner-facing handles. `body_in` is the source side (loop hands a token
		to the body); `body_out` is the target side (body returns a token to
		the loop). Positioning is on the inside of the container so authoring
		intent is visually clear: edges from the inside.
	-->
	<Handle
		id="body_in"
		type="source"
		position={Position.Left}
		class={workflowNodeHandleClass('loop')}
		style="left: 16px; top: 50%;"
	/>
	<Handle
		id="body_out"
		type="target"
		position={Position.Right}
		class={workflowNodeHandleClass('loop')}
		style="right: 16px; top: 50%;"
	/>
</div>
