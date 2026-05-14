<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { EndNodeData } from '$lib/types/editor';
	import Square from '@lucide/svelte/icons/square';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: EndNodeData; selected?: boolean } = $props();
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('end')} />
<WorkflowNodeCard
	nodeId={id}
	kind="end"
	icon={Square}
	label={data.label}
	{selected}
	class="rounded-full px-2"
	data-testid="node-end"
/>
