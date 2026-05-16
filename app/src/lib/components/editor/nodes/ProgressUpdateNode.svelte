<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { ProgressUpdateNodeData } from '$lib/types/editor';
	import Gauge from '@lucide/svelte/icons/gauge';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: ProgressUpdateNodeData; selected?: boolean } =
		$props();

	const pct = $derived(Math.round((data.fraction ?? 0) * 100));
</script>

<Handle
	id="in"
	type="target"
	position={Position.Left}
	class={workflowNodeHandleClass('progress-update')}
/>
<WorkflowNodeCard
	nodeId={id}
	kind="progress-update"
	icon={Gauge}
	label={data.label}
	{selected}
	class="min-w-[170px]"
	data-testid="node-progress-update"
	body={progressBody}
/>
{#snippet progressBody()}
	{pct}%{data.message ? ` · ${data.message}` : ''}
{/snippet}
<Handle
	type="source"
	position={Position.Right}
	class={workflowNodeHandleClass('progress-update')}
/>
