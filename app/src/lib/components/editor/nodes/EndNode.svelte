<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { EndNodeData } from '$lib/types/editor';
	import Square from '@lucide/svelte/icons/square';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { data, selected }: { data: EndNodeData; selected?: boolean } = $props();
</script>

<Handle type="target" position={Position.Left} class={workflowNodeHandleClass('end')} />
<WorkflowNodeCard
	kind="end"
	icon={Square}
	label={data.label}
	{selected}
	class="rounded-full px-2"
	data-testid="node-end"
/>
