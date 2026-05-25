<script lang="ts">
	// Join: unified converge node. `mode` is the explicit knob — `all` waits
	// for every incoming branch and merges payloads (the parallel_join
	// behaviour); `any` fires per arriving token (XOR-join, dual of decision).
	// `mergeStrategy` is only meaningful for `mode === 'all'`.
	import type { JoinNodeData } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';

	const modeLabels: Record<string, string> = {
		all: 'All — wait for every branch (AND-join)',
		any: 'Any — fire on first arrival (XOR-join)'
	};

	const strategyLabels: Record<string, string> = {
		shallow_last_wins: 'Shallow — last branch wins',
		deep_merge: 'Deep — recursively merge nested objects'
	};

	type Props = {
		data: JoinNodeData;
		readonly?: boolean;
		onchange: (data: JoinNodeData) => void;
		binding?: YjsGraphBinding;
		nodeId?: string;
	};

	let { data, readonly = false, onchange, binding, nodeId }: Props = $props();

	const mode = $derived(data.mode ?? 'all');
	const strategy = $derived(data.mergeStrategy ?? 'shallow_last_wins');

	const sources = $derived.by(() => {
		if (!binding || !nodeId) return [] as string[];
		const g = binding.graph;
		const byId = new Map(g.nodes.map((n) => [n.id, n]));
		return g.edges
			.filter((e) => e.target === nodeId)
			.map((e) => byId.get(e.source)?.data.label ?? e.source);
	});
</script>

<div class="space-y-2">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Joins branches</span>
		<span class="text-sm uppercase tracking-wide text-muted-foreground/80">
			{sources.length} input{sources.length === 1 ? '' : 's'}
		</span>
	</div>
	{#if sources.length === 0}
		<p class="rounded-md border border-dashed border-border/50 p-2 text-sm text-muted-foreground">
			Not connected — draw edges from the upstream branches into this join.
		</p>
	{:else}
		<ul class="space-y-1">
			{#each sources as label, i (i)}
				<li class="rounded-md border border-border/60 bg-muted/20 px-2 py-1.5 text-sm text-foreground">
					{label}
				</li>
			{/each}
		</ul>
	{/if}
</div>

<FormField label="Firing rule" for="join-mode">
	<Select.Root
		type="single"
		value={mode}
		onValueChange={(v) => {
			if (!v) return;
			const next = v as 'all' | 'any';
			// Drop mergeStrategy when switching to Any — it's a no-op there.
			const patch: JoinNodeData =
				next === 'any'
					? { ...data, mode: next, mergeStrategy: undefined }
					: { ...data, mode: next, mergeStrategy: data.mergeStrategy ?? 'shallow_last_wins' };
			onchange(patch);
		}}
		disabled={readonly}
	>
		<Select.Trigger
			id="join-mode"
			class="w-full"
			disabled={readonly}
			data-testid="select-join-mode"
		>
			{modeLabels[mode] ?? modeLabels.all}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="all" label="All — wait for every branch (AND-join)" />
			<Select.Item value="any" label="Any — fire on first arrival (XOR-join)" />
		</Select.Content>
	</Select.Root>
</FormField>
<p class="text-sm italic text-muted-foreground">
	{mode === 'any'
		? 'Each arriving branch fires the join independently — only one payload exists per firing.'
		: 'The join blocks until every branch has delivered, then merges their payloads into one token.'}
</p>

{#if mode === 'all'}
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
	<p class="text-sm italic text-muted-foreground">
		{strategy === 'deep_merge'
			? 'Nested object values are merged key-by-key; scalars still take the last branch.'
			: 'Top-level keys are copied in arrival order — the last branch overwrites collisions.'}
	</p>
{/if}
