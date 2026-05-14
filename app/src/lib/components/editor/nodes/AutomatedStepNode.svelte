<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { AutomatedStepNodeData } from '$lib/types/editor';
	import Cpu from '@lucide/svelte/icons/cpu';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: AutomatedStepNodeData; selected?: boolean } = $props();

	// Phase 2 typed-ports: render declared `output` port fields inline so the
	// port editor's effect is visible on the canvas. Falls back to the legacy
	// compact card (just backend name) when output has no declared fields.
	const fields = $derived(data.output?.fields ?? []);
	const hasFields = $derived(fields.length > 0);
	const outputId = $derived(data.output?.id ?? 'out');

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

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('automated')} />
<WorkflowNodeCard
	nodeId={id}
	kind="automated"
	icon={Cpu}
	label={data.label}
	{selected}
	class="min-w-[200px]"
	data-testid="node-automated-step"
	body={automatedBody}
/>
{#snippet automatedBody()}
	<div class="space-y-1.5" data-testid="automated-step-body">
		<div class="truncate capitalize text-foreground/80">
			{data.executionSpec?.backendType ?? 'python'}
		</div>
		{#if hasFields}
			<div class="space-y-0.5 border-t border-border/40 pt-1.5">
				<div class="flex items-center justify-between">
					<span class="text-[10px] uppercase tracking-wider text-muted-foreground/70">
						{data.output?.label ?? 'Output'}
					</span>
					<span class="text-[10px] text-muted-foreground/70">
						{fields.length} field{fields.length === 1 ? '' : 's'}
					</span>
				</div>
				<ul class="space-y-0.5">
					{#each fields as field (field.name)}
						<li class="flex items-center justify-between gap-2">
							<span class="truncate font-mono text-[11px] text-foreground">
								{field.name || '—'}{field.required ? '*' : ''}
							</span>
							<span
								class="rounded bg-node-automated/15 px-1.5 py-0.5 text-[9px] font-medium uppercase text-node-automated"
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
	id={outputId}
	type="source"
	position={Position.Right}
	class={workflowNodeHandleClass('automated')}
/>
