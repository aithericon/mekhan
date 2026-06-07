<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { TriggerNodeData } from '$lib/types/editor';
	import Zap from '@lucide/svelte/icons/zap';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';
	import { NODE_WIDTH } from '$lib/editor/node-dimensions';

	let { id, data, selected }: { id: string; data: TriggerNodeData; selected?: boolean } = $props();

	// Source kind is the visible payload on a trigger card — users care more
	// about "this is a cron" than the node label. Fall back to the label when
	// the source is missing (shouldn't happen post-default-data-factory).
	const sourceKind = $derived(data.source?.kind ?? 'manual');
	const sourceLabel: Record<string, string> = {
		cron: 'Cron',
		catalog: 'Catalog',
		net_completion: 'On Completion',
		webhook: 'Webhook',
		manual: 'API call'
	};
	const subtitle = $derived(sourceLabel[sourceKind] ?? sourceKind);
	const enabled = $derived(data.enabled ?? false);
</script>

<WorkflowNodeCard
	nodeId={id}
	kind="decision"
	icon={Zap}
	label={data.label}
	{selected}
	width={NODE_WIDTH.trigger}
	class={enabled ? undefined : 'opacity-60'}
	data-testid="node-trigger"
	body={triggerBody}
/>

{#snippet triggerBody()}
	<div class="flex items-center justify-between" data-testid="trigger-body">
		<span class="text-sm font-medium uppercase tracking-wider text-muted-foreground">
			{subtitle}
		</span>
		{#if !enabled}
			<span class="rounded-full bg-muted px-1.5 py-0.5 text-sm uppercase tracking-wide text-muted-foreground">
				Disabled
			</span>
		{/if}
	</div>
{/snippet}

<Handle
	id="out"
	type="source"
	position={Position.Right}
	class={workflowNodeHandleClass('decision')}
/>
