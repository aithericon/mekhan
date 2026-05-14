<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { ParallelJoinNodeData } from '$lib/types/editor';
	import GitMerge from '@lucide/svelte/icons/git-merge';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { data, selected }: { data: ParallelJoinNodeData; selected?: boolean } = $props();
</script>

<Handle type="target" position={Position.Left} class={workflowNodeHandleClass('parallel')} />
<WorkflowNodeCard
	kind="parallel"
	icon={GitMerge}
	label={data.label}
	{selected}
	data-testid="node-parallel-join"
/>
<Handle type="source" position={Position.Right} class={workflowNodeHandleClass('parallel')} />
