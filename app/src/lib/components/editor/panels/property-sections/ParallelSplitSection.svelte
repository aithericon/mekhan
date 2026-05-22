<script lang="ts">
	// Read-only structural summary for a ParallelSplit: which downstream nodes
	// the token is forked to. The split itself has no configurable behaviour
	// (it duplicates the input to every outgoing edge); this panel just makes
	// the fan-out legible so users don't have to trace edges on the canvas.
	import type { ParallelSplitNodeData } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';

	type Props = {
		data: ParallelSplitNodeData;
		binding?: YjsGraphBinding;
		nodeId?: string;
	};

	let { binding, nodeId }: Props = $props();

	const targets = $derived.by(() => {
		if (!binding || !nodeId) return [] as string[];
		const g = binding.graph;
		const byId = new Map(g.nodes.map((n) => [n.id, n]));
		return g.edges
			.filter((e) => e.source === nodeId)
			.map((e) => byId.get(e.target)?.data.label ?? e.target);
	});
</script>

<div class="space-y-2">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Fans out to</span>
		<span class="text-sm uppercase tracking-wide text-muted-foreground/80">
			{targets.length} branch{targets.length === 1 ? '' : 'es'}
		</span>
	</div>
	{#if targets.length === 0}
		<p class="rounded-md border border-dashed border-border/50 p-2 text-sm text-muted-foreground">
			Not connected — draw edges to the parallel branches.
		</p>
	{:else}
		<ul class="space-y-1">
			{#each targets as label, i (i)}
				<li class="rounded-md border border-border/60 bg-muted/20 px-2 py-1.5 text-sm text-foreground">
					{label}
				</li>
			{/each}
		</ul>
	{/if}
	<p class="text-sm italic text-muted-foreground">
		The input token is duplicated to every branch.
	</p>
</div>
