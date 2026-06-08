<script lang="ts">
	import { browser } from '$app/environment';
	import { listDataEntries, type DataEntry, type DataEntriesResponse } from '$lib/api/data';
	import {
		getCatalogueStats,
		getCatalogueDistinct,
		getCatalogueDistinctJsonb,
		type CatalogueStats
	} from '$lib/api/client';
	import { ArtifactCard } from '$lib/components/catalogue';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Separator } from '$lib/components/ui/separator';
	import * as Select from '$lib/components/ui/select';
	import Search from '@lucide/svelte/icons/search';
	import FileBox from '@lucide/svelte/icons/file-box';
	import HardDrive from '@lucide/svelte/icons/hard-drive';
	import BarChart3 from '@lucide/svelte/icons/bar-chart-3';
	import ArrowUpDown from '@lucide/svelte/icons/arrow-up-down';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import FileQuestion from '@lucide/svelte/icons/file-question';
	import Server from '@lucide/svelte/icons/server';
	import Database from '@lucide/svelte/icons/database';

	let { onViewServer }: { onViewServer: (key?: string) => void } = $props();

	let resp = $state<DataEntriesResponse | null>(null);
	let stats = $state<CatalogueStats | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let page = $state(0);

	// Filters
	let activeCategory = $state('all');
	let searchQuery = $state('');
	let sourceNetFilter = $state('');
	let formatFilter = $state('');
	let schemaFilter = $state('');
	let sortField = $state('-created_at');

	// Dynamic dropdown facets (from the catalogue distinct endpoints).
	let sourceNets = $state<string[]>([]);
	let categories = $state<string[]>([]);
	let formats = $state<string[]>([]);

	// Inspected artifact — driven by ?inspect= query param (parity w/ old page).
	let inspectId = $state<string | null>(
		browser ? new URLSearchParams(window.location.search).get('inspect') : null
	);
	let showUncatalogued = $state(false);

	const sortOptions = [
		{ value: '-created_at', label: 'Newest first' },
		{ value: 'created_at', label: 'Oldest first' },
		{ value: '-size_bytes', label: 'Largest first' },
		{ value: 'size_bytes', label: 'Smallest first' },
		{ value: 'name', label: 'Name A-Z' },
		{ value: '-name', label: 'Name Z-A' }
	];

	const categoryColors = ['model', 'dataset', 'plot', 'log', 'checkpoint', 'config', 'metric', 'legacy', 'other'];

	const statusColors: Record<string, string> = {
		indexed: 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300',
		verified: 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		registered: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		copied: 'bg-cyan-100 text-cyan-800 dark:bg-cyan-900 dark:text-cyan-200'
	};
	const statusColor = (s: string) =>
		statusColors[s] ?? 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300';

	function formatBytes(bytes: number | null): string {
		if (bytes === null || bytes === undefined) return '—';
		if (bytes === 0) return '0 B';
		const units = ['B', 'KB', 'MB', 'GB', 'TB'];
		const i = Math.floor(Math.log(bytes) / Math.log(1024));
		return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
	}

	// `entry.id` is the catalogue row id (job-net artifacts); content-addressed /
	// by-reference rows carry only entry_id/content_hash, so prefer those.
	const entryKey = (e: DataEntry) =>
		e.entry_id ?? e.content_hash ?? `${e.execution_id}/${e.id}`;

	function toggleInspect(id: string) {
		inspectId = inspectId === id ? null : id;
		if (browser) {
			const url = new URL(window.location.href);
			if (inspectId) url.searchParams.set('inspect', inspectId);
			else url.searchParams.delete('inspect');
			history.replaceState(null, '', url.toString());
		}
	}

	function resetPage() {
		page = 0;
	}

	async function load(
		cat: string, search: string, net: string, fmt: string, schema: string, sort: string, pg: number
	) {
		loading = true;
		error = null;
		try {
			const fmObj: Record<string, unknown> = {};
			if (fmt) fmObj.format = fmt;
			if (schema) fmObj.schema_fingerprint = { digest: schema };
			const fileMetaFilter = Object.keys(fmObj).length > 0 ? JSON.stringify(fmObj) : undefined;
			const [listResult, statsResult] = await Promise.all([
				listDataEntries({
					category: cat === 'all' ? undefined : cat,
					search: search.trim() || undefined,
					source_net: net.trim() || undefined,
					file_metadata: fileMetaFilter,
					sort,
					page: pg,
					page_size: 25
				}),
				getCatalogueStats()
			]);
			resp = listResult;
			stats = statsResult;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load data entries';
			resp = null;
		} finally {
			loading = false;
		}
	}

	async function loadDropdowns() {
		try {
			const [nets, cats, fmts] = await Promise.all([
				getCatalogueDistinct('source_net'),
				getCatalogueDistinct('category'),
				getCatalogueDistinctJsonb('file_metadata', 'format')
			]);
			sourceNets = nets;
			categories = cats;
			formats = fmts;
		} catch {
			// Non-fatal — dropdowns just stay empty.
		}
	}

	let debounce: ReturnType<typeof setTimeout> | undefined;
	$effect(() => {
		const cat = activeCategory, search = searchQuery, net = sourceNetFilter;
		const fmt = formatFilter, schema = schemaFilter, sort = sortField, pg = page;
		clearTimeout(debounce);
		debounce = setTimeout(() => load(cat, search, net, fmt, schema, sort, pg), 300);
		return () => clearTimeout(debounce);
	});

	$effect(() => {
		loadDropdowns();
	});
</script>

<!-- Stats cards (absorbed from the catalogue page) -->
{#if stats}
	<div class="mb-6 grid grid-cols-3 gap-3">
		<div class="rounded-lg border border-border bg-card px-4 py-3">
			<div class="flex items-center gap-2 text-muted-foreground">
				<FileBox class="size-4" />
				<span class="text-sm font-medium uppercase tracking-wide">Total artifacts</span>
			</div>
			<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">{stats.total_entries.toLocaleString()}</p>
		</div>
		<div class="rounded-lg border border-border bg-card px-4 py-3">
			<div class="flex items-center gap-2 text-muted-foreground">
				<HardDrive class="size-4" />
				<span class="text-sm font-medium uppercase tracking-wide">Total size</span>
			</div>
			<p class="mt-1 text-2xl font-semibold text-foreground">{formatBytes(stats.total_size_bytes)}</p>
		</div>
		<div class="rounded-lg border border-border bg-card px-4 py-3">
			<div class="flex items-center gap-2 text-muted-foreground">
				<BarChart3 class="size-4" />
				<span class="text-sm font-medium uppercase tracking-wide">Categories</span>
			</div>
			<div class="mt-1 flex flex-wrap gap-x-2 gap-y-0.5">
				{#each stats.by_category as cat}
					<button class="text-sm text-muted-foreground hover:text-foreground" onclick={() => { activeCategory = cat.category; resetPage(); }}>
						{cat.category}: <span class="font-semibold text-foreground">{cat.count}</span>
					</button>
				{/each}
			</div>
		</div>
	</div>
{/if}

<Separator class="mb-4" />

<!-- Filters -->
<div class="mb-4 space-y-3">
	<div class="flex flex-wrap gap-1">
		<Button variant={activeCategory === 'all' ? 'default' : 'ghost'} size="sm" onclick={() => { activeCategory = 'all'; resetPage(); }}>All</Button>
		{#each (categories.length > 0 ? categories : categoryColors) as cat}
			<Button variant={activeCategory === cat ? 'default' : 'ghost'} size="sm" onclick={() => { activeCategory = cat; resetPage(); }}>
				{cat.charAt(0).toUpperCase() + cat.slice(1)}
			</Button>
		{/each}
	</div>

	<div class="flex flex-wrap items-center gap-2">
		<div class="relative min-w-[14rem] flex-1">
			<Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
			<Input type="text" placeholder="Search name or content hash…" class="h-8 pl-8 text-sm" bind:value={searchQuery} oninput={resetPage} data-testid="data-search" />
		</div>

		{#if sourceNets.length > 0}
			<Select.Root type="single" value={sourceNetFilter} onValueChange={(v) => { sourceNetFilter = v ?? ''; resetPage(); }}>
				<Select.Trigger class="h-8 w-44 text-sm">{sourceNetFilter || 'All nets'}</Select.Trigger>
				<Select.Content>
					<Select.Item value="" label="All nets" />
					{#each sourceNets as net}<Select.Item value={net} label={net} />{/each}
				</Select.Content>
			</Select.Root>
		{/if}

		{#if formats.length > 0}
			<Select.Root type="single" value={formatFilter} onValueChange={(v) => { formatFilter = v ?? ''; resetPage(); }}>
				<Select.Trigger class="h-8 w-28 text-sm">{formatFilter || 'All formats'}</Select.Trigger>
				<Select.Content>
					<Select.Item value="" label="All formats" />
					{#each formats as fmt}<Select.Item value={fmt} label={fmt} />{/each}
				</Select.Content>
			</Select.Root>
		{/if}

		<Select.Root type="single" value={sortField} onValueChange={(v) => { if (v) { sortField = v; resetPage(); } }}>
			<Select.Trigger class="h-8 w-40 text-sm">
				<div class="flex items-center gap-1.5">
					<ArrowUpDown class="size-3.5 text-muted-foreground" />
					{sortOptions.find((o) => o.value === sortField)?.label ?? 'Sort'}
				</div>
			</Select.Trigger>
			<Select.Content>
				{#each sortOptions as opt}<Select.Item value={opt.value} label={opt.label} />{/each}
			</Select.Content>
		</Select.Root>
	</div>

	{#if schemaFilter}
		<div class="flex items-center gap-2">
			<span class="text-sm text-muted-foreground">Schema filter:</span>
			<Badge variant="secondary" class="gap-1 font-mono text-sm">
				{schemaFilter.slice(0, 12)}
				<button class="ml-1 hover:text-foreground" onclick={() => { schemaFilter = ''; resetPage(); }}>&times;</button>
			</Badge>
		</div>
	{/if}
</div>

{#if error}
	<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-200">{error}</div>
{/if}

{#if loading && !resp}
	<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">Loading…</div>
{:else if resp && resp.items.length === 0 && resp.uncatalogued.length === 0}
	<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
		<Database class="size-10 text-muted-foreground/40" />
		<p class="mt-3 text-sm text-muted-foreground">No catalogued content</p>
		<p class="text-sm text-muted-foreground">Artifacts are catalogued when workflow executions produce output</p>
	</div>
{:else if resp}
	<div class="space-y-2">
		{#each resp.items as entry (entryKey(entry))}
			{@const key = entryKey(entry)}
			<ArtifactCard
				{entry}
				copies={entry.copies}
				expanded={inspectId === key}
				highlighted={inspectId === key}
				onToggle={() => toggleInspect(key)}
				onSchemaClick={(digest) => { schemaFilter = digest; resetPage(); }}
				onNetClick={(net) => { sourceNetFilter = net; resetPage(); }}
				onViewServer={() => onViewServer()}
			/>
		{/each}
	</div>

	<!-- Pagination -->
	{#if resp.total_pages > 1}
		<div class="mt-4 flex items-center justify-between">
			<p class="text-sm text-muted-foreground">Showing {resp.items.length} of {resp.total.toLocaleString()} entries</p>
			<div class="flex items-center gap-1">
				<Button variant="ghost" size="icon-sm" disabled={!resp.has_previous} onclick={() => (page = page - 1)}><ChevronLeft class="size-4" /></Button>
				<span class="px-2 text-sm tabular-nums text-muted-foreground">{resp.page + 1} / {resp.total_pages}</span>
				<Button variant="ghost" size="icon-sm" disabled={!resp.has_next} onclick={() => (page = page + 1)}><ChevronRight class="size-4" /></Button>
			</div>
		</div>
	{:else if resp.total > 0}
		<p class="mt-4 text-center text-sm text-muted-foreground">{resp.total.toLocaleString()} {resp.total === 1 ? 'entry' : 'entries'}</p>
	{/if}

	<!-- Uncatalogued (index-only) files -->
	{#if resp.uncatalogued_count > 0}
		<div class="mt-6 rounded-lg border border-dashed border-border">
			<button class="flex w-full items-center gap-2 px-4 py-2.5 text-left" onclick={() => (showUncatalogued = !showUncatalogued)}>
				{#if showUncatalogued}<ChevronDown class="size-3.5 text-muted-foreground" />{:else}<ChevronRight class="size-3.5 text-muted-foreground" />{/if}
				<FileQuestion class="size-4 text-muted-foreground" />
				<span class="text-sm font-medium text-foreground">Uncatalogued files</span>
				<Badge variant="secondary">{resp.uncatalogued_count.toLocaleString()}</Badge>
				<span class="text-sm text-muted-foreground">— observed on disk, not yet hashed/registered</span>
			</button>
			{#if showUncatalogued}
				<div class="space-y-1 border-t border-border px-4 py-2.5">
					{#each resp.uncatalogued as u}
						{@const c = u.copies[0]}
						<div class="flex items-center gap-2 text-sm">
							<span class="truncate font-medium text-foreground">{u.name}</span>
							{#if c}
								<button class="inline-flex items-center gap-1 text-muted-foreground hover:text-foreground" onclick={() => onViewServer(c.file_server_id)}>
									<Server class="size-3" /><span>{c.server_display_name ?? c.file_server_id}</span>
								</button>
								<span class="truncate font-mono text-muted-foreground" title={c.path}>{c.path}</span>
								<Badge class={statusColor(c.status)} variant="secondary">{c.status}</Badge>
							{/if}
						</div>
					{/each}
					{#if resp.uncatalogued_count > resp.uncatalogued.length}
						<p class="pt-1 text-sm text-muted-foreground">…and {(resp.uncatalogued_count - resp.uncatalogued.length).toLocaleString()} more</p>
					{/if}
				</div>
			{/if}
		</div>
	{/if}
{/if}
