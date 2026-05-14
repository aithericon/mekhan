<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { StartNodeData } from '$lib/types/editor';
	import Play from '@lucide/svelte/icons/play';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { data, selected }: { data: StartNodeData; selected?: boolean } = $props();

	// Phase 1 typed-ports: render declared `initial` port fields inline so the
	// port editor's effect is visible on the canvas. When no fields are
	// declared we fall back to the compact pill style (existing behavior for
	// legacy / empty-port Starts).
	const fields = $derived(data.initial?.fields ?? []);
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

{#if hasFields}
	<WorkflowNodeCard
		kind="start"
		icon={Play}
		label={data.label}
		{selected}
		class="min-w-[200px]"
		data-testid="node-start"
		body={portBody}
	/>
{:else}
	<WorkflowNodeCard
		kind="start"
		icon={Play}
		label={data.label}
		{selected}
		class="rounded-full px-2"
		data-testid="node-start"
	/>
{/if}

{#snippet portBody()}
	<div class="space-y-1" data-testid="start-port-fields">
		<div class="flex items-center justify-between">
			<span class="text-[10px] uppercase tracking-wider text-muted-foreground/70">
				{data.initial?.label ?? 'Input'}
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
						class="rounded bg-node-start/15 px-1.5 py-0.5 text-[9px] font-medium uppercase text-node-start"
					>
						{kindBadge[field.kind] ?? field.kind}
					</span>
				</li>
			{/each}
		</ul>
	</div>
{/snippet}

<Handle
	id={data.initial?.id ?? 'in'}
	type="source"
	position={Position.Right}
	class={workflowNodeHandleClass('start')}
/>
