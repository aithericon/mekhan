<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { ParallelSplitNodeData } from '$lib/types/editor';
	import GitFork from '@lucide/svelte/icons/git-fork';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { data, selected }: { data: ParallelSplitNodeData; selected?: boolean } = $props();
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('parallel')} />
<WorkflowNodeCard
	kind="parallel"
	icon={GitFork}
	label={data.label}
	{selected}
	data-testid="node-parallel-split"
/>
<Handle type="source" position={Position.Right} class={workflowNodeHandleClass('parallel')} />
