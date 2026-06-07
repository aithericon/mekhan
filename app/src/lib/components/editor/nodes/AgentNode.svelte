<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { AgentNodeData } from '$lib/types/editor';
	import Bot from '@lucide/svelte/icons/bot';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';
	import { outputPortsFor } from '$lib/editor/derived-ports';
	import { NODE_WIDTH } from '$lib/editor/node-dimensions';

	let { id, data, selected }: { id: string; data: AgentNodeData; selected?: boolean } = $props();

	const maxTurns = $derived(data.maxTurns ?? 1);
	const provider = $derived(data.model?.provider ?? 'anthropic');
	const modelName = $derived(data.model?.model ?? '');
	const stopWhen = $derived(data.stopWhen ?? null);
	const policy = $derived(data.onToolError ?? 'feedback');
	const isSingleShot = $derived(maxTurns <= 1 && !stopWhen);

	// Derived output port — same source the panel reads. Always shows the
	// canonical four LLM fields; loop-path agents (max_turns > 1 OR
	// stop_when set) show the four extras too. Compiler is the source of
	// truth; this just renders.
	const successPort = $derived(outputPortsFor(data)[0]);
	const fields = $derived(successPort?.fields ?? []);
	const hasFields = $derived(fields.length > 0);

	const kindBadge: Record<string, string> = {
		text: 'Txt',
		textarea: 'Txt',
		number: 'Num',
		bool: 'Bool',
		select: 'Sel',
		file: 'File',
		signature: 'Sig',
		timestamp: 'Time',
		json: 'JSON'
	};
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('agent')} />
<!-- Tools handle: drag from here to any AutomatedStep / SubWorkflow / etc.
     node that should be callable by the LLM. The compiler discovers tool
     children by walking outgoing edges with sourceHandle="tools"; the
     LLM-facing tool name + description come from the target node's own
     `label` (slugified) and `description` — no separate side-channel.
     Distinct purple style + top placement keeps it visually separated
     from the data flow (`out` right, `error` bottom). -->
<Handle
	id="tools"
	type="source"
	position={Position.Top}
	class="!h-3 !w-3 !border-2"
	style="background:#a855f7;border-color:#7e22ce;"
	title="Connect to tool nodes the agent can call"
/>
<WorkflowNodeCard
	nodeId={id}
	kind="agent"
	icon={Bot}
	label={data.label}
	{selected}
	width={NODE_WIDTH.agent}
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
		{#if hasFields}
			<div class="space-y-0.5 border-t border-border/40 pt-1.5">
				<div class="flex items-center justify-between">
					<span class="text-sm uppercase tracking-wider text-muted-foreground/70">
						{successPort?.label ?? 'Output'}
					</span>
					<span class="text-sm text-muted-foreground/70">
						{fields.length} field{fields.length === 1 ? '' : 's'}
					</span>
				</div>
				<ul class="space-y-0.5">
					{#each fields as field (field.name)}
						<li class="flex items-center justify-between gap-2">
							<span class="truncate font-mono text-sm text-foreground">
								{field.name || '—'}{field.required ? '*' : ''}
							</span>
							<span
								class="rounded bg-node-agent/15 px-1.5 py-0.5 text-sm font-medium uppercase text-node-agent"
							>
								{kindBadge[field.kind] ?? field.kind}
							</span>
						</li>
					{/each}
				</ul>
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
	class="!h-3 !w-3 !border-2"
	style="background:#ef4444;border-color:#b91c1c;"
	title="On agent failure"
/>
