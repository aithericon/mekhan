<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { StartNodeData } from '$lib/types/editor';
	import Play from '@lucide/svelte/icons/play';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { data, selected }: { data: StartNodeData; selected?: boolean } = $props();
</script>

<WorkflowNodeCard
	kind="start"
	icon={Play}
	label={data.label}
	{selected}
	class="rounded-full px-2"
	data-testid="node-start"
/>
<Handle type="source" position={Position.Right} class={workflowNodeHandleClass('start')} />
