<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { PhaseUpdateNodeData } from '$lib/types/editor';
	import Flag from '@lucide/svelte/icons/flag';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: PhaseUpdateNodeData; selected?: boolean } =
		$props();
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('phase-update')} />
<WorkflowNodeCard
	nodeId={id}
	kind="phase-update"
	icon={Flag}
	label={data.label}
	{selected}
	class="min-w-[170px]"
	data-testid="node-phase-update"
	body={phaseBody}
/>
{#snippet phaseBody()}
	{data.phaseName || 'Unnamed phase'} · {data.status ?? 'running'}
{/snippet}
<Handle type="source" position={Position.Right} class={workflowNodeHandleClass('phase-update')} />
