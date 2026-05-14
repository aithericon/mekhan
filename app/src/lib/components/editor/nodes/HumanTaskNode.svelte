<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { HumanTaskNodeData } from '$lib/types/editor';
	import User from '@lucide/svelte/icons/user';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: HumanTaskNodeData; selected?: boolean } = $props();

	const stepCount = $derived(data.steps?.length ?? 0);
	const fieldCount = $derived(
		data.steps?.reduce((sum, step) => sum + step.blocks.filter((b) => b.type === 'input').length, 0) ?? 0
	);
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('human-task')} />
<WorkflowNodeCard
	nodeId={id}
	kind="human-task"
	icon={User}
	label={data.label}
	{selected}
	class="min-w-[180px]"
	data-testid="node-human-task"
	body={humanTaskBody}
/>
{#snippet humanTaskBody()}
	<div class="truncate font-medium">{data.taskTitle || 'Untitled task'}</div>
	{#if stepCount > 0 || fieldCount > 0}
		<div class="mt-0.5 text-muted-foreground/80">
			{stepCount} step{stepCount !== 1 ? 's' : ''}, {fieldCount} field{fieldCount !== 1 ? 's' : ''}
		</div>
	{/if}
{/snippet}
<Handle type="source" position={Position.Right} class={workflowNodeHandleClass('human-task')} />
