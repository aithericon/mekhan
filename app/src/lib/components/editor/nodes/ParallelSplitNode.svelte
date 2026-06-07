<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { ParallelSplitNodeData } from '$lib/types/editor';
	import GitFork from '@lucide/svelte/icons/git-fork';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';
	import { NODE_WIDTH } from '$lib/editor/node-dimensions';

	let { id, data, selected }: { id: string; data: ParallelSplitNodeData; selected?: boolean } = $props();
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('parallel')} />
<WorkflowNodeCard
	nodeId={id}
	kind="parallel"
	icon={GitFork}
	label={data.label}
	{selected}
	width={NODE_WIDTH.parallel_split}
	data-testid="node-parallel-split"
/>
<Handle type="source" position={Position.Right} class={workflowNodeHandleClass('parallel')} />
