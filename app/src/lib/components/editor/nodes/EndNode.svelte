<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { EndNodeData } from '$lib/types/editor';
	import Square from '@lucide/svelte/icons/square';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';

	let { id, data, selected }: { id: string; data: EndNodeData; selected?: boolean } = $props();

	// Mirror StartNode's typed-port rendering: when result mappings are
	// declared, list them inline so the success-envelope shape is visible on
	// the canvas. Each row is `targetField ← <expression>` — the published key
	// is the heading, the borrowed source the muted chip on the right.
	const mappings = $derived(data.resultMapping ?? []);
	const hasMappings = $derived(mappings.length > 0);
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('end')} />

{#if hasMappings}
	<WorkflowNodeCard
		nodeId={id}
		kind="end"
		icon={Square}
		label={data.label}
		{selected}
		class="min-w-[200px]"
		data-testid="node-end"
		body={resultBody}
	/>
{:else}
	<WorkflowNodeCard
		nodeId={id}
		kind="end"
		icon={Square}
		label={data.label}
		{selected}
		class="rounded-full px-2"
		data-testid="node-end"
	/>
{/if}

{#snippet resultBody()}
	<div class="space-y-1" data-testid="end-result-fields">
		<div class="flex items-center justify-between">
			<span class="text-sm uppercase tracking-wider text-muted-foreground/70">Result</span>
			<span class="text-sm text-muted-foreground/70">
				{mappings.length} field{mappings.length === 1 ? '' : 's'}
			</span>
		</div>
		<ul class="space-y-0.5">
			{#each mappings as m, i (i)}
				<li class="flex items-center justify-between gap-2">
					<span class="truncate font-mono text-sm text-foreground">
						{m.targetField || '—'}
					</span>
					<span
						class="truncate rounded bg-node-end/15 px-1.5 py-0.5 font-mono text-sm text-node-end"
						title={m.expression}
					>
						{m.expression || '—'}
					</span>
				</li>
			{/each}
		</ul>
	</div>
{/snippet}
