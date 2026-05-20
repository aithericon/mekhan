<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { StartNodeData } from '$lib/types/editor';
	import Play from '@lucide/svelte/icons/play';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: StartNodeData; selected?: boolean } = $props();

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

<!-- Trigger entrypoints (cron/catalog/webhook/...) fire the workflow by
     connecting their single outgoing edge into the Start's `initial` port.
     This target handle is what xyflow drops that edge onto; its id must equal
     the initial port id so the backend resolves it via Start's output_ports()
     (see validate_triggers / triggers::dispatcher). Without it the editor has
     nowhere to land a Trigger→Start edge. -->
<Handle
	id={data.initial?.id ?? 'in'}
	type="target"
	position={Position.Left}
	class={workflowNodeHandleClass('start')}
/>

{#if hasFields}
	<WorkflowNodeCard
		nodeId={id}
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
		nodeId={id}
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
			<span class="text-sm uppercase tracking-wider text-muted-foreground/70">
				{data.initial?.label ?? 'Input'}
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
						class="rounded bg-node-start/15 px-1.5 py-0.5 text-sm font-medium uppercase text-node-start"
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
