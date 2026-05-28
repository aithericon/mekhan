<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { JoinNodeData } from '$lib/types/editor';
	import GitMerge from '@lucide/svelte/icons/git-merge';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: JoinNodeData; selected?: boolean } = $props();

	const mode = $derived(data.mode ?? 'all');
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('parallel')} />
<WorkflowNodeCard
	nodeId={id}
	kind="parallel"
	icon={GitMerge}
	label={data.label}
	{selected}
	data-testid="node-join"
>
	{#snippet body()}
		<span
			class="inline-flex items-center rounded-md bg-muted/40 px-1.5 py-0.5 text-sm font-medium uppercase tracking-wide text-muted-foreground"
		>
			{mode === 'any' ? 'Any' : 'All'}
		</span>
	{/snippet}
</WorkflowNodeCard>
<Handle type="source" position={Position.Right} class={workflowNodeHandleClass('parallel')} />
