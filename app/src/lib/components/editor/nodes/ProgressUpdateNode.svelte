<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { ProgressUpdateNodeData } from '$lib/types/editor';
	import Gauge from '@lucide/svelte/icons/gauge';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';
	import { NODE_WIDTH } from '$lib/editor/node-dimensions';

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
	width={NODE_WIDTH.progress_update}
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
