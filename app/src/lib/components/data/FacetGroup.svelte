<script lang="ts">
	// One collapsible facet dimension in the Entries query rail: bucket rows
	// (label + count) OF THE CURRENT QUERY SCOPE, click to narrow further.
	// Fetching is gated on `expanded` — collapsed groups never read `query`,
	// so they neither fetch on mount nor refetch on apply (the column /
	// classification dimensions are LATERAL jsonb unnests server-side; don't
	// pay for groups the user never opens).
	import { getCatalogueFacets, type FacetBucket } from '$lib/api/data';
	import { formatCount } from './format';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';

	let {
		dim,
		label,
		termPrefix,
		query,
		onAdd,
		defaultExpanded = false
	}: {
		/** Server facet dimension (`group_by` value). */
		dim: string;
		label: string;
		/** DSL prefix for bucket clicks, e.g. `col` → `col:email`. */
		termPrefix: string;
		/** The current applied query text (buckets scope to it). */
		query: string;
		/** Called with the DSL term to add, e.g. `format:csv`. */
		onAdd: (term: string) => void;
		defaultExpanded?: boolean;
	} = $props();

	// svelte-ignore state_referenced_locally — initial value only, by design
	let expanded = $state(defaultExpanded);
	let buckets = $state<FacetBucket[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	// Skip the redundant refetch when a group is collapsed + re-expanded
	// without the query having changed in between.
	let loadedFor: string | null = null;

	async function load(q: string) {
		loading = true;
		error = null;
		try {
			// Raw DSL → server-side compiler (canonical filter scope).
			const resp = await getCatalogueFacets({ group_by: dim, limit: 30, q });
			buckets = resp.buckets;
			loadedFor = q;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load facets';
			buckets = [];
			loadedFor = null;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (!expanded) return;
		const q = query;
		if (q !== loadedFor) load(q);
	});

	function termFor(key: string): string {
		const value = /[\s"]/.test(key) ? `"${key}"` : key;
		return `${termPrefix}:${value}`;
	}
</script>

<div data-testid={`facet-group-${dim}`}>
	<button
		type="button"
		class="flex w-full items-center gap-1.5 rounded px-1 py-0.5 text-sm font-medium text-foreground hover:bg-accent"
		onclick={() => (expanded = !expanded)}
		data-testid={`facet-group-${dim}-toggle`}
	>
		{#if expanded}
			<ChevronDown class="size-3.5 text-muted-foreground" />
		{:else}
			<ChevronRight class="size-3.5 text-muted-foreground" />
		{/if}
		{label}
	</button>

	{#if expanded}
		<div class="mt-1 max-h-56 space-y-px overflow-y-auto pl-2">
			{#if loading}
				<div class="space-y-1 py-0.5" aria-hidden="true">
					{#each [20, 14, 24] as w (w)}
						<div class="h-5 animate-pulse rounded bg-muted" style={`width: ${w * 4}px`}></div>
					{/each}
				</div>
			{:else if error}
				<p class="px-2 py-1 text-xs text-rose-600 dark:text-rose-400">{error}</p>
			{:else if buckets.length === 0}
				<p class="px-2 py-1 text-xs text-muted-foreground">no facets in scope</p>
			{:else}
				{#each buckets as b (b.key)}
					<button
						type="button"
						class="flex w-full items-center justify-between gap-2 rounded px-2 py-1 text-left text-sm text-foreground hover:bg-accent"
						title={termFor(b.key)}
						onclick={() => onAdd(termFor(b.key))}
					>
						<span class="min-w-0 truncate">{b.key}</span>
						<span class="shrink-0 tabular-nums text-xs text-muted-foreground">{formatCount(b.count)}</span>
					</button>
				{/each}
			{/if}
		</div>
	{/if}
</div>
