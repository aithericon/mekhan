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
	import { parseQuery, quoteIfNeeded } from './query-language';
	import type { EntriesQueryState } from './entries-query.svelte';
	import type { DataTypesState } from './data-types.svelte';
	import QueryBar from './QueryBar.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { StatusBadge } from '$lib/components/status';
	import { Button } from '$lib/components/ui/button';
	import * as Select from '$lib/components/ui/select';
	import FileBox from '@lucide/svelte/icons/file-box';
	import HardDrive from '@lucide/svelte/icons/hard-drive';
	import ArrowUpDown from '@lucide/svelte/icons/arrow-up-down';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import FileQuestion from '@lucide/svelte/icons/file-question';
	import Server from '@lucide/svelte/icons/server';
	import Database from '@lucide/svelte/icons/database';

	let {
		entries,
		datatypes,
		onViewServer
	}: {
		/** Shared query state — the page owns it; the rail holds the other half. */
		entries: EntriesQueryState;
		/** Registered data types — `datatype:` terms compile through its resolver. */
		datatypes: DataTypesState;
		onViewServer: (key?: string) => void;
	} = $props();

	let resp = $state<DataEntriesResponse | null>(null);
	let stats = $state<CatalogueStats | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let sortField = $state('-created_at');

	// Known filter fields for the QueryBar's unknown-field validation — the
	// server registry (native + meta names), fetched once (module-cached).
	let knownFields = $state<Set<string> | null>(null);
	$effect(() => {
		getCatalogueQueryFields()
			.then((r) => {
				knownFields = new Set([...r.native, ...r.meta].map((f) => f.name));
			})
			.catch(() => {});
	});

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

	// `entry.id` is the catalogue row id (job-net artifacts); content-addressed /
	// by-reference rows carry only entry_id/content_hash, so prefer those.
	const entryKey = (e: DataEntry) =>
		e.entry_id ?? e.content_hash ?? `${e.execution_id}/${e.id}`;

	function setInspect(id: string | null) {
		inspectId = id;
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
			// Submit the raw DSL as `q` — a single server-side compiler resolves it
			// (relative dates re-resolved per request, `datatype:`/`format:`/contain
			// sugars unwrapped server-side). The TS compiler no longer drives the
			// request; it stays for the live chip/validation UX only.
			const [listResult, statsResult] = await Promise.all([
				listDataEntries({
					q,
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
		const q = entries.applied, sort = sortField, pg = entries.page;
		// `datatype:` terms resolve through the registry, which may land AFTER
		// the first load — read `datatypes.list` synchronously here so this
		// effect re-runs (and recompiles past the fail-closed params) when it does.
		if (parseQuery(q).terms.some((t) => t.kind === 'datatype')) void datatypes.list;
		clearTimeout(debounce);
		debounce = setTimeout(() => load(q, sort, pg), 300);
		return () => clearTimeout(debounce);
	});
</script>

<!-- Query bar, then one toolbar row: catalogue totals left, sort right.
     Facets / saved queries / field reference live in the rail (EntriesRail). -->
<div class="mb-4 space-y-3">
	<QueryBar {entries} {knownFields} datatypeNames={datatypes.loading ? null : datatypes.names} />

	<div class="flex flex-wrap items-center gap-x-4 gap-y-2">
		{#if stats}
			<div class="flex items-center gap-x-4 text-sm text-muted-foreground" data-testid="entries-stat-line">
				<span class="inline-flex items-center gap-1.5">
					<FileBox class="size-3.5" />
					<span class="font-medium tabular-nums text-foreground">{stats.total_entries.toLocaleString()}</span>
					artifacts
				</span>
				<span class="inline-flex items-center gap-1.5">
					<HardDrive class="size-3.5" />
					<span class="font-medium text-foreground">{formatBytes(stats.total_size_bytes)}</span>
				</span>
			</div>
		{/if}

		<div class="ml-auto">
			<Select.Root type="single" value={sortField} onValueChange={(v) => { if (v) { sortField = v; entries.page = 0; } }}>
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
				detailsOpen={inspectId === key}
				highlighted={inspectId === key}
				onDetailsOpenChange={(open) => setInspect(open ? key : null)}
				onSchemaClick={(digest) => {
					const dt = datatypes.byDigest.get(digest);
					entries.addTerm(dt ? `datatype:${quoteIfNeeded(dt.name)}` : `meta.schema:${digest}`);
				}}
				onNetClick={(net) => entries.addTerm(`source_net:${net}`)}
				onViewServer={(key) => onViewServer(key)}
			/>
		{/each}
	</div>

	<!-- Pagination -->
	{#if resp.total_pages > 1}
		<div class="mt-4 flex items-center justify-between">
			<p class="text-sm text-muted-foreground">Showing {resp.items.length} of {resp.total.toLocaleString()} entries</p>
			<div class="flex items-center gap-1">
				<Button variant="ghost" size="icon-sm" disabled={!resp.has_previous} onclick={() => (entries.page = entries.page - 1)}><ChevronLeft class="size-4" /></Button>
				<span class="px-2 text-sm tabular-nums text-muted-foreground">{resp.page + 1} / {resp.total_pages}</span>
				<Button variant="ghost" size="icon-sm" disabled={!resp.has_next} onclick={() => (entries.page = entries.page + 1)}><ChevronRight class="size-4" /></Button>
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
								<StatusBadge domain="copy" status={c.status} />
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
