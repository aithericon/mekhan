<script lang="ts">
	// Slim facet strip under the QueryBar: pick a dimension, see the bucket
	// distribution OF THE CURRENT QUERY SCOPE (the strip compiles the same
	// query text the result list uses), click a bucket to narrow further.
	import { getCatalogueFacets, type FacetBucket } from '$lib/api/data';
	import { parseQuery, compileQuery } from './query-language';
	import { formatCount } from './format';
	import { FilterPills } from '$lib/components/shell';

	let {
		query,
		onAdd
	}: {
		/** The current applied query text (the strip scopes facets to it). */
		query: string;
		/** Called with the DSL term to add, e.g. `format:csv`. */
		onAdd: (term: string) => void;
	} = $props();

	const DIMENSIONS = [
		{ value: 'format', label: 'Format' },
		{ value: 'category', label: 'Category' },
		{ value: 'source_net', label: 'Net' },
		{ value: 'column', label: 'Column' },
		{ value: 'classification', label: 'PII' }
	];

	// Bucket key → DSL term per dimension.
	const TERM_PREFIX: Record<string, string> = {
		format: 'format',
		category: 'category',
		source_net: 'source_net',
		column: 'col',
		classification: 'pii'
	};

	let dimension = $state('format');
	let buckets = $state<FacetBucket[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	async function load(dim: string, q: string) {
		loading = true;
		error = null;
		try {
			const compiled = compileQuery(parseQuery(q).terms);
			const resp = await getCatalogueFacets({
				group_by: dim,
				limit: 30,
				search: compiled.search,
				filters: compiled.filters,
				file_metadata: compiled.fileMetadata ? JSON.stringify(compiled.fileMetadata) : undefined
			});
			buckets = resp.buckets;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load facets';
			buckets = [];
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		const dim = dimension,
			q = query;
		load(dim, q);
	});

	function termFor(key: string): string {
		const value = /[\s"]/.test(key) ? `"${key}"` : key;
		return `${TERM_PREFIX[dimension] ?? dimension}:${value}`;
	}
</script>

<div class="border-b border-border pb-3" data-testid="facet-strip">
	<div class="flex flex-wrap items-center gap-2">
		<FilterPills
			options={DIMENSIONS}
			active={dimension}
			onSelect={(v) => (dimension = v)}
			testid="facet-strip-dimension"
		/>

		<div class="flex min-w-0 flex-1 items-center gap-1.5 overflow-x-auto">
			{#if loading}
				<div class="flex items-center gap-1.5" aria-hidden="true">
					{#each [16, 12, 20, 14] as w (w)}
						<div class="h-6 animate-pulse rounded-full bg-muted" style={`width: ${w * 4}px`}></div>
					{/each}
				</div>
			{:else if error}
				<p class="text-xs text-rose-600 dark:text-rose-400">{error}</p>
			{:else if buckets.length === 0}
				<p class="text-xs text-muted-foreground">no facets</p>
			{:else}
				{#each buckets as b (b.key)}
					<button
						type="button"
						class="inline-flex shrink-0 items-center gap-1 rounded-full border border-border bg-card px-2 py-0.5 text-xs text-foreground transition-colors hover:bg-accent"
						title={termFor(b.key)}
						onclick={() => onAdd(termFor(b.key))}
					>
						<span class="max-w-48 truncate">{b.key}</span>
						<span class="tabular-nums text-muted-foreground">{formatCount(b.count)}</span>
					</button>
				{/each}
			{/if}
		</div>
	</div>
</div>
