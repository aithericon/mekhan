<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { LoopNodeData } from '$lib/types/editor';
	import Repeat from '@lucide/svelte/icons/repeat';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { data, selected }: { data: LoopNodeData; selected?: boolean } = $props();
</script>

<Handle type="target" position={Position.Left} class={workflowNodeHandleClass('loop')} />
<WorkflowNodeCard
	kind="loop"
	icon={Repeat}
	label={data.label}
	{selected}
	class="min-w-[160px]"
	data-testid="node-loop"
	body={loopBody}
/>
{#snippet loopBody()}
	Max {data.maxIterations ?? 3} iterations
{/snippet}
<Handle type="source" position={Position.Right} class={workflowNodeHandleClass('loop')} />
