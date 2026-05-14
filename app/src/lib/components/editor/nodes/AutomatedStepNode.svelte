<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import Cpu from '@lucide/svelte/icons/cpu';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { data, selected }: { data: AutomatedStepNodeData; selected?: boolean } = $props();
</script>

<Handle type="target" position={Position.Left} class={workflowNodeHandleClass('automated')} />
<WorkflowNodeCard
	kind="automated"
	icon={Cpu}
	label={data.label}
	{selected}
	class="min-w-[180px]"
	data-testid="node-automated-step"
	body={automatedBody}
/>
{#snippet automatedBody()}
	<div class="truncate capitalize">{data.executionSpec?.backendType ?? 'python'}</div>
{/snippet}
<Handle type="source" position={Position.Right} class={workflowNodeHandleClass('automated')} />
