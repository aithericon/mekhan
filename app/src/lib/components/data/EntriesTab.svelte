<script lang="ts">
	import { browser } from '$app/environment';
	import {
		listDataEntries,
		getCatalogueQueryFields,
		type DataEntry,
		type DataEntriesResponse
	} from '$lib/api/data';
	import { getCatalogueStats, type CatalogueStats } from '$lib/api/client';
	import { ArtifactCard } from '$lib/components/catalogue';
	import { formatBytes } from './format';
	import { parseQuery, compileQuery, formatQuery, addTerm } from './query-language';
	import QueryBar from './QueryBar.svelte';
	import FacetStrip from './FacetStrip.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Separator } from '$lib/components/ui/separator';
	import * as Select from '$lib/components/ui/select';
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

	// ── ONE source of truth for the filter state: the query text (?q=) ───────
	let queryText = $state(
		browser ? (new URLSearchParams(window.location.search).get('q') ?? '') : ''
	);
	let sortField = $state('-created_at');

	// Known filter fields for the QueryBar's unknown-field validation — the
	// server registry (native + meta names), fetched once.
	let knownFields = $state<Set<string> | null>(null);
	$effect(() => {
		getCatalogueQueryFields()
			.then((r) => {
				knownFields = new Set([...r.native, ...r.meta].map((f) => f.name));
			})
			.catch(() => {});
	});

	function syncUrl(text: string) {
		if (!browser) return;
		const url = new URL(window.location.href);
		if (text.trim()) url.searchParams.set('q', text);
		else url.searchParams.delete('q');
		history.replaceState(null, '', url.toString());
	}

	/** Apply new query text: reset paging + sync ?q= (same pattern as ?inspect). */
	function applyQuery(text: string) {
		queryText = text;
		page = 0;
		syncUrl(text);
	}

	function addAndApply(term: string) {
		applyQuery(addTerm(queryText, term));
	}

	// ── Category pills write/remove a `category:x` term on the query text ────
	const activeCategory = $derived.by(() => {
		const { terms } = parseQuery(queryText);
		const t = terms.find((t) => t.kind === 'filter' && t.field === 'category' && t.op === 'eq');
		return t && t.kind === 'filter' ? t.value : 'all';
	});

	function setCategory(cat: string) {
		const p = parseQuery(queryText);
		const kept = p.terms.filter((t) => !(t.kind === 'filter' && t.field === 'category'));
		let text = [formatQuery(kept), ...p.errors.map((e) => e.raw)].filter(Boolean).join(' ');
		if (cat !== 'all') text = addTerm(text, `category:${cat}`);
		applyQuery(text);
	}

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
		{ value: '-name', label: 'Name Z-A' },
		{ value: '-meta.num_rows', label: 'Most rows' },
		{ value: '-meta.completeness', label: 'Most complete' }
	];

	const fallbackCategories = ['model', 'dataset', 'plot', 'log', 'checkpoint', 'config', 'metric', 'file', 'other'];
	const categories = $derived(
		stats && stats.by_category.length > 0 ? stats.by_category.map((c) => c.category) : fallbackCategories
	);

	const statusColors: Record<string, string> = {
		indexed: 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300',
		verified: 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		registered: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		copied: 'bg-cyan-100 text-cyan-800 dark:bg-cyan-900 dark:text-cyan-200'
	};
	const statusColor = (s: string) =>
		statusColors[s] ?? 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300';

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

	async function load(q: string, sort: string, pg: number) {
		loading = true;
		error = null;
		try {
			// Parse errors are excluded from `terms` — fetch with the valid ones.
			const compiled = compileQuery(parseQuery(q).terms);
			const [listResult, statsResult] = await Promise.all([
				listDataEntries({
					search: compiled.search,
					filters: compiled.filters,
					file_metadata: compiled.fileMetadata ? JSON.stringify(compiled.fileMetadata) : undefined,
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

	let debounce: ReturnType<typeof setTimeout> | undefined;
	$effect(() => {
		const q = queryText, sort = sortField, pg = page;
		clearTimeout(debounce);
		debounce = setTimeout(() => load(q, sort, pg), 300);
		return () => clearTimeout(debounce);
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
					<button class="text-sm text-muted-foreground hover:text-foreground" onclick={() => setCategory(cat.category)}>
						{cat.category}: <span class="font-semibold text-foreground">{cat.count}</span>
					</button>
				{/each}
			</div>
		</div>
	</div>
{/if}

<Separator class="mb-4" />

<!-- Filters: category pills + sort, then the query bar, then the facet strip -->
<div class="mb-4 space-y-3">
	<div class="flex flex-wrap items-center gap-2">
		<div class="flex flex-wrap gap-1">
			<Button variant={activeCategory === 'all' ? 'default' : 'ghost'} size="sm" onclick={() => setCategory('all')}>All</Button>
			{#each categories as cat}
				<Button variant={activeCategory === cat ? 'default' : 'ghost'} size="sm" onclick={() => setCategory(cat)}>
					{cat.charAt(0).toUpperCase() + cat.slice(1)}
				</Button>
			{/each}
		</div>

		<div class="ml-auto">
			<Select.Root type="single" value={sortField} onValueChange={(v) => { if (v) { sortField = v; page = 0; } }}>
				<Select.Trigger class="h-8 w-44 text-sm">
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
	</div>

	<QueryBar value={queryText} onApply={applyQuery} {knownFields} />

	<FacetStrip query={queryText} onAdd={addAndApply} />
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
				onSchemaClick={(digest) => addAndApply(`meta.schema:${digest}`)}
				onNetClick={(net) => addAndApply(`source_net:${net}`)}
				onViewServer={(key) => onViewServer(key)}
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
