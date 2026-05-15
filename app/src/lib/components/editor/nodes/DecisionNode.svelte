<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { DecisionNodeData } from '$lib/types/editor';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: DecisionNodeData; selected?: boolean } = $props();

	const branchCount = $derived(
		(data.conditions?.length ?? 0) + (data.defaultBranch ? 1 : 0)
	);
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('decision')} />
<WorkflowNodeCard
	nodeId={id}
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
{#if data.defaultBranch}
	<Handle
		type="source"
		position={Position.Right}
		id="default"
		class={workflowNodeHandleClass('decision')}
	/>
{/if}
{#each data.conditions ?? [] as condition, i (condition.edgeId)}
	<Handle
		type="source"
		position={Position.Right}
		id={condition.edgeId}
		style="top: {30 + (i + 1) * 20}px;"
		class={workflowNodeHandleClass('decision')}
	/>
{/each}
