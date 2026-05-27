<script lang="ts">
	import { onMount } from 'svelte';
	import { loadNodeTypes, nodeList } from '$lib/editor/node-registry.svelte';
	import { nodePaletteMeta } from '$lib/editor/node-palette-meta';
	import type { WorkflowNodeType } from '$lib/types/editor';

	onMount(() => {
		// Idempotent — the +layout.svelte warmup already fired, but if a route
		// renders the palette before the layout resolves we fetch here too.
		loadNodeTypes().catch(() => { /* surfaced via empty list */ });
	});

	const items = $derived(nodeList());

	function onDragStart(event: DragEvent, nodeType: WorkflowNodeType) {
		if (!event.dataTransfer) return;
		event.dataTransfer.setData('application/mekhan-node-type', nodeType);
		event.dataTransfer.effectAllowed = 'move';
	}
</script>

<div class="flex w-56 flex-col border-r border-sidebar-border bg-sidebar" data-testid="node-palette">
	<div class="border-b border-sidebar-border px-3 py-2.5">
		<h2 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">Blocks</h2>
	</div>
	<div class="flex-1 space-y-1 overflow-y-auto p-2">
		{#each items as item (item.wireName)}
			{@const meta = nodePaletteMeta(item.wireName)}
			{@const Icon = meta.icon}
			<!-- svelte-ignore a11y_no_static_element_interactions -->
			<div
				class="group flex cursor-grab items-center gap-2.5 rounded-lg border border-transparent px-2.5 py-2 text-sm transition-colors hover:border-border hover:bg-accent active:cursor-grabbing"
				draggable="true"
				data-testid="palette-item-{item.wireName}"
				ondragstart={(e) => onDragStart(e, item.wireName as WorkflowNodeType)}
			>
				<div
					class="flex size-7 shrink-0 items-center justify-center rounded-md"
					style="background-color: {meta.color}20; color: {meta.color};"
				>
					<Icon class="size-4" />
				</div>
				<div class="min-w-0">
					<div class="text-sm font-medium text-foreground">{item.displayLabel}</div>
					{#if item.description}
						<div class="truncate text-sm leading-tight text-muted-foreground">
							{item.description}
						</div>
					{/if}
				</div>
			</div>
		{/each}
	</div>
</div>
