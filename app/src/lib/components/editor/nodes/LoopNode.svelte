<script lang="ts">
	import { Handle, NodeResizer, Position } from '@xyflow/svelte';
	import type { LoopNodeData } from '$lib/types/editor';
	import Repeat from '@lucide/svelte/icons/repeat';
	import { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { data, selected }: { data: LoopNodeData; selected?: boolean } = $props();

	// Header reads as `while {cond} · max N`. The default condition is the
	// literal `true` — render that as "forever" so the safety cap reads as
	// the real stop, which matches how engine actually halts the loop.
	const COND_MAX = 28;
	let condText = $derived.by(() => {
		const c = (data.loopCondition ?? '').trim();
		if (!c || c === 'true') return 'forever';
		return c.length > COND_MAX ? `while ${c.slice(0, COND_MAX - 1)}…` : `while ${c}`;
	});
	let maxText = $derived(`max ${data.maxIterations ?? 3}`);
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

<!--
	Outer perimeter handles: connect the loop to the parent flow.
	Rendered as outline circles (default workflowNodeHandleClass — `bg-card`
	fill, colored border) to distinguish them from the filled body handles
	below.
-->
<Handle
	id="in"
	type="target"
	position={Position.Left}
	class={workflowNodeHandleClass('loop')}
	title="Flow in — enter loop"
/>
<Handle
	id="out"
	type="source"
	position={Position.Right}
	class={workflowNodeHandleClass('loop')}
	title="Flow out — continue after loop"
/>

<div
	class="relative h-full w-full rounded-xl border-2 border-dashed {selected
		? 'border-node-loop bg-node-loop/10'
		: 'border-node-loop/50 bg-node-loop/5'}"
	data-testid="node-loop"
>
	<!--
		Title bar — sits in the top inside edge so it doesn't eat the perimeter
		handle area. Reads as `<label> · while {cond} · max N`, with the
		condition as the primary stop semantic and the cap as a muted
		secondary suffix. The default condition (`true`) renders as
		"forever" so the cap reads as the real stop.
	-->
	<div class="absolute left-3 top-2 flex items-center gap-2 text-sm font-medium text-muted-foreground">
		<Repeat class="size-4 text-node-loop" />
		<span>{data.label}</span>
		<span class="font-mono text-sm">· {condText}</span>
		<span class="text-sm opacity-60">· {maxText}</span>
	</div>

	<!--
		Inner-facing handles. `body_in` is the source side (loop hands a token
		to the body); `body_out` is the target side (body returns a token to
		the loop). Positioned 16px inside the container's left/right edges so
		they read as "inside" handles, and styled with a SOLID fill (vs the
		outline-style outer perimeter handles) so the four ports are
		visually unambiguous.
	-->
	<Handle
		id="body_in"
		type="source"
		position={Position.Left}
		class="!h-3 !w-3 !border-2 !border-node-loop !bg-node-loop"
		style="left: 16px; top: 50%;"
		title="Body in — start of each iteration"
	/>
	<Handle
		id="body_out"
		type="target"
		position={Position.Right}
		class="!h-3 !w-3 !border-2 !border-node-loop !bg-node-loop"
		style="right: 16px; top: 50%;"
		title="Body out — end of each iteration (returns to loop)"
	/>
</div>
