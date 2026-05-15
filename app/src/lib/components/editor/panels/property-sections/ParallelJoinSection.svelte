<script lang="ts">
	// ParallelJoin: shows the branches being merged and lets the user pick how
	// colliding keys are resolved. `shallow_last_wins` matches the historical
	// compiler behaviour; `deep_merge` recursively merges nested objects. The
	// chosen strategy is honoured by the compiler's `build_join_merge_logic`.
	import type { ParallelJoinNodeData } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';

	const strategyLabels: Record<string, string> = {
		shallow_last_wins: 'Shallow — last branch wins',
		deep_merge: 'Deep — recursively merge nested objects'
	};

	type Props = {
		data: ParallelJoinNodeData;
		readonly?: boolean;
		onchange: (data: ParallelJoinNodeData) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
	};

	let { data, readonly = false, onchange, binding, nodeId }: Props = $props();

	const sources = $derived.by(() => {
		if (!binding || !nodeId) return [] as string[];
		const g = binding.graph;
		const byId = new Map(g.nodes.map((n) => [n.id, n]));
		return g.edges
			.filter((e) => e.target === nodeId)
			.map((e) => byId.get(e.source)?.data.label ?? e.source);
	});

	const strategy = $derived(data.mergeStrategy ?? 'shallow_last_wins');
</script>

<div class="space-y-2">
	<div class="flex items-center justify-between">
		<span class="text-xs font-medium text-muted-foreground">Joins branches</span>
		<span class="text-[10px] uppercase tracking-wide text-muted-foreground/80">
			{sources.length} input{sources.length === 1 ? '' : 's'}
		</span>
	</div>
	{#if sources.length === 0}
		<p class="rounded-md border border-dashed border-border/50 p-2 text-[11px] text-muted-foreground">
			Not connected — draw edges from the parallel branches into this join.
		</p>
	{:else}
		<ul class="space-y-1">
			{#each sources as label, i (i)}
				<li class="rounded-md border border-border/60 bg-muted/20 px-2 py-1.5 text-[11px] text-foreground">
					{label}
				</li>
			{/each}
		</ul>
	{/if}
</div>

<FormField label="Merge strategy" for="merge-strategy">
	<Select.Root
		type="single"
		value={strategy}
		onValueChange={(v) => {
			if (v) onchange({ ...data, mergeStrategy: v as 'shallow_last_wins' | 'deep_merge' });
		}}
		disabled={readonly}
	>
		<Select.Trigger
			id="merge-strategy"
			class="w-full"
			disabled={readonly}
			data-testid="select-merge-strategy"
		>
			{strategyLabels[strategy] ?? strategyLabels.shallow_last_wins}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="shallow_last_wins" label="Shallow — last branch wins" />
			<Select.Item value="deep_merge" label="Deep — recursively merge nested objects" />
		</Select.Content>
	</Select.Root>
</FormField>
<p class="text-[10px] italic text-muted-foreground">
	{strategy === 'deep_merge'
		? 'Nested object values are merged key-by-key; scalars still take the last branch.'
		: 'Top-level keys are copied in arrival order — the last branch overwrites collisions.'}
</p>
