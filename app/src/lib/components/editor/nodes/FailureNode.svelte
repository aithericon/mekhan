<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { FailureNodeData } from '$lib/types/editor';
	import OctagonX from '@lucide/svelte/icons/octagon-x';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: FailureNodeData; selected?: boolean } =
		$props();
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('failure')} />
<WorkflowNodeCard
	nodeId={id}
	kind="failure"
	icon={OctagonX}
	label={data.label}
	{selected}
	class="min-w-[170px]"
	data-testid="node-failure"
	body={failureBody}
/>
{#snippet failureBody()}
	{data.failureMessage || 'No message'}
{/snippet}
<Handle type="source" position={Position.Right} class={workflowNodeHandleClass('failure')} />
