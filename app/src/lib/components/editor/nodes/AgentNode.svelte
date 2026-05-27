<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { AgentNodeData } from '$lib/types/editor';
	import Bot from '@lucide/svelte/icons/bot';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: AgentNodeData; selected?: boolean } = $props();

	const maxTurns = $derived(data.maxTurns ?? 1);
	const provider = $derived(data.model?.provider ?? 'anthropic');
	const modelName = $derived(data.model?.model ?? '');
	const stopWhen = $derived(data.stopWhen ?? null);
	const policy = $derived(data.onToolError ?? 'feedback');
	const isSingleShot = $derived(maxTurns <= 1 && !stopWhen);
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('agent')} />
<WorkflowNodeCard
	nodeId={id}
	kind="agent"
	icon={Bot}
	label={data.label}
	{selected}
	class="min-w-[220px]"
	data-testid="node-agent"
	body={agentBody}
/>
{#snippet agentBody()}
	<div class="space-y-1.5" data-testid="agent-body">
		<div class="flex items-center justify-between gap-2">
			<span class="truncate font-mono text-sm text-foreground/80" title="{provider} / {modelName}">
				{provider}
			</span>
			<span class="shrink-0 rounded bg-node-agent/15 px-1.5 py-0.5 text-sm font-medium uppercase text-node-agent">
				{isSingleShot ? '1 turn' : `≤${maxTurns} turns`}
			</span>
		</div>
		{#if modelName}
			<div class="truncate font-mono text-sm text-muted-foreground" title={modelName}>
				{modelName}
			</div>
		{/if}
		{#if !isSingleShot}
			<div class="flex items-center justify-between gap-2 border-t border-border/40 pt-1.5">
				<span class="text-sm uppercase tracking-wider text-muted-foreground/70">on tool error</span>
				<span class="rounded bg-node-agent/10 px-1.5 py-0.5 text-sm font-medium text-node-agent">
					{policy}
				</span>
			</div>
		{/if}
	</div>
{/snippet}
<Handle
	id="out"
	type="source"
	position={Position.Right}
	class={workflowNodeHandleClass('agent')}
/>
<!-- Error output: the agent's LLM call dead-ended (retries exhausted) or a
     tool bubbled. Wire to a handler / End or it dead-ends. -->
<Handle
	id="error"
	type="source"
	position={Position.Bottom}
	style="background:#ef4444;border-color:#b91c1c;"
	title="On agent failure"
/>
