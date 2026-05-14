<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { DecisionNodeData } from '$lib/types/editor';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { data, selected }: { data: DecisionNodeData; selected?: boolean } = $props();

	const branchCount = $derived((data.conditions?.length ?? 0) + 1); // +1 for default
</script>

<Handle type="target" position={Position.Left} class={workflowNodeHandleClass('decision')} />
<WorkflowNodeCard
	kind="decision"
	icon={GitBranch}
	label={data.label}
	{selected}
	class="min-w-[160px]"
	data-testid="node-decision"
	body={branchBody}
/>
{#snippet branchBody()}
	{branchCount} branch{branchCount !== 1 ? 'es' : ''}
{/snippet}
<Handle
	type="source"
	position={Position.Right}
	id="default"
	class={workflowNodeHandleClass('decision')}
/>
{#each data.conditions ?? [] as condition, i (condition.edgeId)}
	<Handle
		type="source"
		position={Position.Right}
		id={condition.edgeId}
		style="top: {30 + (i + 1) * 20}px;"
		class={workflowNodeHandleClass('decision')}
	/>
{/each}
