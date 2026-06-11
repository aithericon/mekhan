<script lang="ts">
	// The Schemas facet group — the one dimension the dumb FacetGroup can't
	// render: bucket keys are schema-fingerprint digests joined client-side
	// against the registered data types (registered → named, click inserts a
	// `datatype:` term; unregistered → raw digest + a Promote affordance).
	// The lazy-fetch contract is copied verbatim from FacetGroup: collapsed
	// groups never read `query`, so they neither fetch on mount nor refetch
	// on apply; `loadedFor` dedupes collapse/re-expand.
	import { getCatalogueFacets, type FacetBucket } from '$lib/api/data';
	import { parseQuery, compileQuery, quoteIfNeeded } from './query-language';
	import type { DataTypesState } from './data-types.svelte';
	import DataTypeDialog from './DataTypeDialog.svelte';
	import { formatCount } from './format';
	import { Badge } from '$lib/components/ui/badge';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import Tag from '@lucide/svelte/icons/tag';

	let {
		query,
		datatypes,
		onAdd
	}: {
		/** The current applied query text (buckets scope to it). */
		query: string;
		/** Registered data types — digest→name join + `datatype:` resolution. */
		datatypes: DataTypesState;
		/** Called with the DSL term to add. */
		onAdd: (term: string) => void;
	} = $props();

	let expanded = $state(false);
	let buckets = $state<FacetBucket[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	// Skip the redundant refetch when a group is collapsed + re-expanded
	// without the query having changed in between.
	let loadedFor: string | null = null;

	let dialog = $state<DataTypeDialog>();

	async function load(q: string) {
		loading = true;
		error = null;
		try {
			const compiled = compileQuery(parseQuery(q).terms, undefined, datatypes.resolveDigests);
			const resp = await getCatalogueFacets({
				group_by: 'schema',
				limit: 30,
				search: compiled.search,
				filters: compiled.filters,
				file_metadata: compiled.fileMetadata ? JSON.stringify(compiled.fileMetadata) : undefined
			});
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

	function addFor(key: string) {
		const dt = datatypes.byDigest.get(key);
		onAdd(dt ? `datatype:${quoteIfNeeded(dt.name)}` : `meta.schema:${key}`);
	}

	// hex16 digests get truncated; the `none` placeholder bucket stays as-is.
	const shortDigest = (key: string) => (key.length > 12 ? `${key.slice(0, 10)}…` : key);
</script>

<div data-testid="facet-group-schema">
	<button
		type="button"
		class="flex w-full items-center gap-1.5 rounded px-1 py-0.5 text-sm font-medium text-foreground hover:bg-accent"
		onclick={() => (expanded = !expanded)}
		data-testid="facet-group-schema-toggle"
	>
		{#if expanded}
			<ChevronDown class="size-3.5 text-muted-foreground" />
		{:else}
			<ChevronRight class="size-3.5 text-muted-foreground" />
		{/if}
		Schemas
	</button>

	{#if expanded}
		<div class="mt-1 max-h-56 space-y-px overflow-y-auto pl-2" data-testid="facet-group-schema-rows">
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
					{@const dt = datatypes.byDigest.get(b.key)}
					<div class="flex w-full items-center gap-1 rounded hover:bg-accent">
						<button
							type="button"
							class="flex min-w-0 flex-1 items-center justify-between gap-2 px-2 py-1 text-left text-sm text-foreground"
							title={dt ? `datatype:${dt.name}` : b.key}
							onclick={() => addFor(b.key)}
						>
							{#if dt}
								<span class="flex min-w-0 items-center gap-1.5">
									<span class="min-w-0 truncate">{dt.name}</span>
									<Badge variant="outline" class="shrink-0 px-1 py-0 text-[10px]">type</Badge>
								</span>
							{:else}
								<span class="min-w-0 truncate font-mono text-xs">{shortDigest(b.key)}</span>
							{/if}
							<span class="shrink-0 tabular-nums text-xs text-muted-foreground">{formatCount(b.count)}</span>
						</button>
						{#if !dt && b.key !== 'none'}
							<button
								type="button"
								class="shrink-0 rounded p-1 text-muted-foreground hover:text-foreground"
								title="Register as data type…"
								onclick={() => dialog?.openPromote(b.key)}
								data-testid="facet-schema-promote"
							>
								<Tag class="size-3.5" />
							</button>
						{/if}
					</div>
				{/each}
			{/if}
		</div>
	{/if}
</div>

<DataTypeDialog bind:this={dialog} {datatypes} />
