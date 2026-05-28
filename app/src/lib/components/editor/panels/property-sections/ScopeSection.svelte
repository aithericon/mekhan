<script lang="ts">
	// Scope: a visual grouping that compiles to a ScenarioGroup (children are
	// tagged with its group_id). It has no runtime behaviour of its own, so
	// this panel just lists the contained nodes for orientation.
	import type { ScopeNodeData } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	type Props = {
		data: ScopeNodeData;
		readonly?: boolean;
		onchange: (data: ScopeNodeData) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
	};

	// `readonly`/`onchange` are part of the uniform section contract (every
	// section receives them via the NodePropertyPanel registry dispatch).
	// Scope is a pure visual grouping with no runtime config, so they're
	// accepted but unused.
	let { binding, nodeId }: Props = $props();

	const children = $derived.by(() => {
		if (!binding || !nodeId) return [] as { label: string; type: string }[];
		return binding.graph.nodes
			.filter((n) => n.parentId === nodeId)
			.map((n) => ({ label: n.data.label, type: n.data.type }));
	});
</script>

<div class="space-y-2">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Contains</span>
		<span class="text-sm uppercase tracking-wide text-muted-foreground/80">
			{children.length} node{children.length === 1 ? '' : 's'}
		</span>
	</div>
	{#if children.length === 0}
		<p class="rounded-md border border-dashed border-border/50 p-2 text-sm text-muted-foreground">
			Empty — drag nodes inside this scope to group them.
		</p>
	{:else}
		<ul class="space-y-1">
			{#each children as child, i (i)}
				<li class="flex items-center justify-between rounded-md border border-border/60 bg-muted/20 px-2 py-1.5 text-sm">
					<span class="text-foreground">{child.label}</span>
					<span class="font-mono text-sm text-muted-foreground">{child.type}</span>
				</li>
			{/each}
		</ul>
	{/if}
</div>
