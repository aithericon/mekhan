<script lang="ts">
	import { browser } from '$app/environment';
	import {
		listCatalogueEntries,
		getCatalogueStats,
		getCatalogueDistinct,
		getCatalogueDistinctJsonb,
		catalogueDownloadUrl
	} from '$lib/api/client';
	import type { CatalogueEntry, CatalogueStats } from '$lib/types/catalogue';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Select from '$lib/components/ui/select';
	import { Separator } from '$lib/components/ui/separator';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import Database from '@lucide/svelte/icons/database';
	import HardDrive from '@lucide/svelte/icons/hard-drive';
	import FileBox from '@lucide/svelte/icons/file-box';
	import Search from '@lucide/svelte/icons/search';
	import BarChart3 from '@lucide/svelte/icons/bar-chart-3';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import Download from '@lucide/svelte/icons/download';
	import ArrowUpDown from '@lucide/svelte/icons/arrow-up-down';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';

	// ── State ──────────────────────────────────────────────────────────────────
	let entries = $state<CatalogueEntry[]>([]);
	let stats = $state<CatalogueStats | null>(null);
	let total = $state(0);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let page = $state(0);
	let pageSize = $state(20);
	let totalPages = $state(0);
	let hasNext = $state(false);
	let hasPrevious = $state(false);

	// Filters
	let activeCategory = $state<string>('all');
	let searchQuery = $state('');
	let sourceNetFilter = $state('');
	let formatFilter = $state('');
	let sortField = $state('-created_at');

	// Inspected artifact — driven by ?inspect= query param
	let inspectId = $state<string | null>(
		browser ? new URLSearchParams(window.location.search).get('inspect') : null
	);

	// Dynamic dropdown values
	let sourceNets = $state<string[]>([]);
	let categories = $state<string[]>([]);
	let formats = $state<string[]>([]);

	// ── Category config ────────────────────────────────────────────────────────
	const categoryColors: Record<string, string> = {
		model: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		dataset: 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		plot: 'bg-purple-100 text-purple-800 dark:bg-purple-900 dark:text-purple-200',
		log: 'bg-gray-100 text-gray-700 dark:bg-gray-800 dark:text-gray-300',
		checkpoint: 'bg-orange-100 text-orange-800 dark:bg-orange-900 dark:text-orange-200',
		config: 'bg-cyan-100 text-cyan-800 dark:bg-cyan-900 dark:text-cyan-200',
		metric: 'bg-rose-100 text-rose-800 dark:bg-rose-900 dark:text-rose-200',
		other: 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300'
	};

	const sortOptions = [
		{ value: '-created_at', label: 'Newest first' },
		{ value: 'created_at', label: 'Oldest first' },
		{ value: '-size_bytes', label: 'Largest first' },
		{ value: 'size_bytes', label: 'Smallest first' },
		{ value: 'name', label: 'Name A-Z' },
		{ value: '-name', label: 'Name Z-A' }
	];

	// ── Helpers ────────────────────────────────────────────────────────────────
	function formatBytes(bytes: number | null): string {
		if (bytes === null || bytes === undefined) return '—';
		if (bytes === 0) return '0 B';
		const units = ['B', 'KB', 'MB', 'GB', 'TB'];
		const i = Math.floor(Math.log(bytes) / Math.log(1024));
		return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
	}

	const formatDate = (s: string) =>
		new Intl.DateTimeFormat(undefined, {
			year: 'numeric', month: 'short', day: 'numeric',
			hour: '2-digit', minute: '2-digit'
		}).format(new Date(s));

	function truncatePath(path: string | null, max = 48): string {
		if (!path) return '—';
		return path.length <= max ? path : '…' + path.slice(-(max - 1));
	}

	function truncateCell(val: unknown, max = 40): string {
		if (val === null || val === undefined) return '—';
		const s = typeof val === 'object' ? JSON.stringify(val) : String(val);
		return s.length <= max ? s : s.slice(0, max - 1) + '…';
	}

	type Preview = { columns: string[]; rows: unknown[][]; preview_row_count: number; total_row_count?: number };

	function getPreview(fm: Record<string, unknown>): Preview | null {
		const p = fm.preview as Preview | undefined;
		if (p && Array.isArray(p.columns) && Array.isArray(p.rows) && p.rows.length > 0) return p;
		return null;
	}

	function categoryColor(cat: string): string {
		return categoryColors[cat.toLowerCase()] ?? categoryColors.other;
	}

	function toggleInspect(id: string) {
		if (inspectId === id) {
			inspectId = null;
		} else {
			inspectId = id;
		}
		if (browser) {
			const url = new URL(window.location.href);
			if (inspectId) {
				url.searchParams.set('inspect', inspectId);
			} else {
				url.searchParams.delete('inspect');
			}
			history.replaceState(null, '', url.toString());
		}
	}

	function entryKey(e: CatalogueEntry): string {
		return `${e.execution_id}/${e.id}`;
	}

	function lineageId(e: CatalogueEntry): string | null {
		return e.process_id ?? e.job_id?.split(':')[0] ?? null;
	}

	// ── Data loading ───────────────────────────────────────────────────────────
	async function load(
		cat: string, search: string, net: string, fmt: string,
		sort: string, pg: number, pgSize: number
	) {
		loading = true;
		error = null;
		try {
			const fileMetaFilter = fmt ? JSON.stringify({ format: fmt }) : undefined;
			const [listResult, statsResult] = await Promise.all([
				listCatalogueEntries({
					category: cat === 'all' ? undefined : cat,
					search: search.trim() || undefined,
					source_net: net.trim() || undefined,
					file_metadata: fileMetaFilter,
					sort,
					page: pg,
					page_size: pgSize
				}),
				getCatalogueStats()
			]);
			entries = listResult.items;
			total = listResult.total;
			totalPages = listResult.total_pages;
			hasNext = listResult.has_next;
			hasPrevious = listResult.has_previous;
			stats = statsResult;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load catalogue';
			entries = [];
			total = 0;
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
			// Non-fatal — dropdowns just stay empty
		}
	}

	function resetPage() { page = 0; }

	// Debounce for text inputs
	let debounceTimer: ReturnType<typeof setTimeout> | undefined;

	$effect(() => {
		// Read all reactive deps
		const cat = activeCategory;
		const search = searchQuery;
		const net = sourceNetFilter;
		const fmt = formatFilter;
		const sort = sortField;
		const pg = page;
		const pgSize = pageSize;

		clearTimeout(debounceTimer);
		debounceTimer = setTimeout(() => {
			load(cat, search, net, fmt, sort, pg, pgSize);
		}, 300);

		return () => clearTimeout(debounceTimer);
	});

	// Load dropdown values once on mount
	$effect(() => { loadDropdowns(); });
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">

		<!-- Header -->
		<div class="mb-6">
			<div class="flex items-center gap-2">
				<Database class="size-6 text-muted-foreground" />
				<h1 class="text-2xl font-semibold tracking-tight text-foreground">Data Catalogue</h1>
			</div>
			<p class="mt-1 text-sm text-muted-foreground">
				Artifacts, models, datasets and plots produced by workflow executions
			</p>
		</div>

		<!-- Stats cards -->
		{#if stats}
			<div class="mb-6 grid grid-cols-3 gap-3">
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-muted-foreground">
						<FileBox class="size-4" />
						<span class="text-xs font-medium uppercase tracking-wide">Total artifacts</span>
					</div>
					<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
						{stats.total_entries.toLocaleString()}
					</p>
				</div>
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-muted-foreground">
						<HardDrive class="size-4" />
						<span class="text-xs font-medium uppercase tracking-wide">Total size</span>
					</div>
					<p class="mt-1 text-2xl font-semibold text-foreground">
						{formatBytes(stats.total_size_bytes)}
					</p>
				</div>
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-muted-foreground">
						<BarChart3 class="size-4" />
						<span class="text-xs font-medium uppercase tracking-wide">Categories</span>
					</div>
					<div class="mt-1 flex flex-wrap gap-1">
						{#each stats.by_category as cat}
							<span class="text-xs text-muted-foreground">
								{cat.category}: <span class="font-semibold text-foreground">{cat.count}</span>
							</span>
						{/each}
					</div>
				</div>
			</div>
		{/if}

		<Separator class="mb-4" />

		<!-- Filters bar -->
		<div class="mb-4 space-y-3">
			<!-- Row 1: Category buttons -->
			<div class="flex flex-wrap gap-1">
				<Button
					variant={activeCategory === 'all' ? 'default' : 'ghost'}
					size="sm"
					onclick={() => { activeCategory = 'all'; resetPage(); }}
				>
					All
				</Button>
				{#each categories.length > 0 ? categories : Object.keys(categoryColors) as cat}
					<Button
						variant={activeCategory === cat ? 'default' : 'ghost'}
						size="sm"
						onclick={() => { activeCategory = cat; resetPage(); }}
					>
						{cat.charAt(0).toUpperCase() + cat.slice(1)}
					</Button>
				{/each}
			</div>

			<!-- Row 2: Search, source net dropdown, sort -->
			<div class="flex items-center gap-2">
				<div class="relative flex-1">
					<Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
					<Input
						type="text"
						placeholder="Search artifacts…"
						class="h-8 pl-8 text-sm"
						bind:value={searchQuery}
						oninput={resetPage}
					/>
				</div>

				{#if sourceNets.length > 0}
					<Select.Root
						type="single"
						value={sourceNetFilter}
						onValueChange={(v) => { sourceNetFilter = v ?? ''; resetPage(); }}
					>
						<Select.Trigger class="h-8 w-44 text-sm">
							{sourceNetFilter || 'All nets'}
						</Select.Trigger>
						<Select.Content>
							<Select.Item value="" label="All nets" />
							{#each sourceNets as net}
								<Select.Item value={net} label={net} />
							{/each}
						</Select.Content>
					</Select.Root>
				{:else}
					<Input
						type="text"
						placeholder="Source net…"
						class="h-8 w-36 text-sm"
						bind:value={sourceNetFilter}
						oninput={resetPage}
					/>
				{/if}

				{#if formats.length > 0}
					<Select.Root
						type="single"
						value={formatFilter}
						onValueChange={(v) => { formatFilter = v ?? ''; resetPage(); }}
					>
						<Select.Trigger class="h-8 w-28 text-sm">
							{formatFilter || 'All formats'}
						</Select.Trigger>
						<Select.Content>
							<Select.Item value="" label="All formats" />
							{#each formats as fmt}
								<Select.Item value={fmt} label={fmt} />
							{/each}
						</Select.Content>
					</Select.Root>
				{/if}

				<Select.Root
					type="single"
					value={sortField}
					onValueChange={(v) => { if (v) { sortField = v; resetPage(); } }}
				>
					<Select.Trigger class="h-8 w-40 text-sm">
						<div class="flex items-center gap-1.5">
							<ArrowUpDown class="size-3.5 text-muted-foreground" />
							{sortOptions.find((o) => o.value === sortField)?.label ?? 'Sort'}
						</div>
					</Select.Trigger>
					<Select.Content>
						{#each sortOptions as opt}
							<Select.Item value={opt.value} label={opt.label} />
						{/each}
					</Select.Content>
				</Select.Root>
			</div>
		</div>

		<!-- Error -->
		{#if error}
			<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
				{error}
			</div>
		{/if}

		<!-- Loading -->
		{#if loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading…
			</div>

		<!-- Empty -->
		{:else if entries.length === 0}
			<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
				<Database class="size-10 text-muted-foreground/40" />
				<p class="mt-3 text-sm text-muted-foreground">No catalogue entries</p>
				<p class="text-xs text-muted-foreground">
					Artifacts are catalogued when workflow executions produce output
				</p>
			</div>

		<!-- Results -->
		{:else}
			<div class="space-y-2">
				{#each entries as entry (entryKey(entry))}
					{@const key = entryKey(entry)}
					{@const isInspected = inspectId === key}
					{@const hasMetadata =
						Object.keys(entry.file_metadata).length > 0 ||
						Object.keys(entry.user_metadata).length > 0}

					<div
						class="rounded-lg border bg-card transition-colors {isInspected ? 'border-primary ring-1 ring-primary/30' : 'border-border hover:bg-accent/30'}"
						id="entry-{key}"
					>
						<!-- Main row -->
						<div class="flex items-start justify-between gap-4 p-4">
							<div class="min-w-0 flex-1">
								<div class="flex flex-wrap items-center gap-2">
									<span class="text-sm font-medium text-foreground truncate">
										{entry.name}
									</span>
									<Badge class={categoryColor(entry.category)} variant="secondary">
										{entry.category}
									</Badge>
									{#if entry.file_metadata?.format}
										<Badge variant="outline" class="text-[10px] font-mono">{entry.file_metadata.format}</Badge>
									{:else if entry.mime_type}
										<Badge variant="outline" class="text-[10px] font-mono">{entry.mime_type}</Badge>
									{/if}
									{#if entry.job_id}
										<span class="text-[10px] font-mono text-muted-foreground">{entry.job_id}</span>
									{/if}
								</div>

									<div class="mt-1.5 flex flex-wrap items-center gap-x-4 gap-y-0.5 text-xs text-muted-foreground">
									{#if entry.source_net}
										<span>Net: <span class="font-mono">{entry.source_net}</span></span>
									{/if}
									{#if lineageId(entry)}
										<a
											href="/catalogue/lineage/{lineageId(entry)}?artifact={encodeURIComponent(entry.id)}"
											class="text-primary underline underline-offset-2 hover:text-primary/80"
										>View lineage</a>
									{/if}
									{#if entry.process_step}
										<span>Step: {entry.process_step}</span>
									{/if}
								</div>

								<div class="mt-1 flex flex-wrap items-center gap-x-4 gap-y-0.5 text-[10px] text-muted-foreground">
									<span>{formatDate(entry.created_at)}</span>
									<span class="font-mono" title={entry.storage_path ?? ''}>
										{truncatePath(entry.storage_path)}
									</span>
								</div>
							</div>

							<div class="flex shrink-0 items-center gap-3">
								<span class="text-sm font-medium tabular-nums text-muted-foreground">
									{formatBytes(entry.size_bytes)}
								</span>

								<!-- Download button -->
								{#if entry.storage_path}
									<Tooltip.Root>
										<Tooltip.Trigger>
											<a
												href={catalogueDownloadUrl(entry.storage_path)}
												class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
												download
											>
												<Download class="size-4" />
											</a>
										</Tooltip.Trigger>
										<Tooltip.Content>Download artifact</Tooltip.Content>
									</Tooltip.Root>
								{/if}

								{#if hasMetadata}
									<Tooltip.Root>
										<Tooltip.Trigger>
											<button
												class="inline-flex size-8 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
												onclick={() => toggleInspect(key)}
											>
												{#if isInspected}
													<ChevronDown class="size-4" />
												{:else}
													<ChevronRight class="size-4" />
												{/if}
											</button>
										</Tooltip.Trigger>
										<Tooltip.Content>{isInspected ? 'Hide' : 'Show'} metadata</Tooltip.Content>
									</Tooltip.Root>
								{/if}
							</div>
						</div>

						<!-- Expanded metadata -->
						{#if isInspected && hasMetadata}
							{@const preview = getPreview(entry.file_metadata)}
							<div class="border-t border-border px-4 pb-4 pt-3 space-y-3">

								<!-- Data preview table -->
								{#if preview}
									<div>
										<p class="mb-1.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
											Data preview
											{#if preview.total_row_count}
												<span class="ml-1 font-normal normal-case">
													({preview.preview_row_count} of {preview.total_row_count.toLocaleString()} rows)
												</span>
											{/if}
										</p>
										<div class="overflow-x-auto rounded-md border border-border">
											<table class="w-full text-[11px]">
												<thead>
													<tr class="border-b border-border bg-muted/50">
														{#each preview.columns as col}
															<th class="px-2 py-1.5 text-left font-medium text-muted-foreground whitespace-nowrap">
																{col}
															</th>
														{/each}
													</tr>
												</thead>
												<tbody>
													{#each preview.rows as row, i}
														<tr class={i % 2 === 0 ? 'bg-card' : 'bg-muted/20'}>
															{#each row as cell}
																<td class="px-2 py-1 text-foreground whitespace-nowrap font-mono" title={typeof cell === 'object' ? JSON.stringify(cell) : String(cell ?? '')}>
																	{truncateCell(cell)}
																</td>
															{/each}
														</tr>
													{/each}
												</tbody>
											</table>
										</div>
									</div>
								{/if}

								<!-- Schema (columns) -->
								{#if Array.isArray(entry.file_metadata.columns) && entry.file_metadata.columns.length > 0}
									<div>
										<p class="mb-1.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
											Schema ({entry.file_metadata.columns.length} columns)
										</p>
										<div class="flex flex-wrap gap-1">
											{#each entry.file_metadata.columns as col}
												<span class="inline-flex items-center gap-1 rounded border border-border bg-muted/50 px-1.5 py-0.5 text-[10px]">
													<span class="font-medium text-foreground">{col.name}</span>
													<span class="text-muted-foreground">{typeof col.data_type === 'string' ? col.data_type : JSON.stringify(col.data_type)}</span>
												</span>
											{/each}
										</div>
									</div>
								{/if}

								<!-- File info summary -->
								{#if entry.file_metadata.format || entry.file_metadata.checksum}
									<div class="flex flex-wrap gap-x-4 gap-y-1 text-[10px] text-muted-foreground">
										{#if entry.file_metadata.format}
											<span>Format: <span class="font-mono text-foreground">{entry.file_metadata.format}</span></span>
										{/if}
										{#if entry.file_metadata.num_rows != null}
											<span>Rows: <span class="font-mono text-foreground">{entry.file_metadata.num_rows}</span></span>
										{/if}
										{#if entry.file_metadata.checksum}
											{@const ck = entry.file_metadata.checksum as Record<string, unknown> | undefined}
											<span>SHA-256: <span class="font-mono text-foreground">{String(ck?.digest ?? '').slice(0, 12)}…</span></span>
										{/if}
									</div>
								{/if}

								<!-- User metadata -->
								{#if Object.keys(entry.user_metadata).length > 0}
									<div>
										<p class="mb-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
											User metadata
										</p>
										<pre class="overflow-x-auto rounded-md bg-muted px-3 py-2 text-[11px] text-foreground">{JSON.stringify(entry.user_metadata, null, 2)}</pre>
									</div>
								{/if}
							</div>
						{/if}
					</div>
				{/each}
			</div>

			<!-- Pagination controls -->
			{#if totalPages > 1}
				<div class="mt-4 flex items-center justify-between">
					<p class="text-xs text-muted-foreground">
						Showing {entries.length} of {total.toLocaleString()} entries
					</p>
					<div class="flex items-center gap-1">
						<Button
							variant="ghost"
							size="icon-sm"
							disabled={!hasPrevious}
							onclick={() => (page = page - 1)}
						>
							<ChevronLeft class="size-4" />
						</Button>
						<span class="px-2 text-xs tabular-nums text-muted-foreground">
							{page + 1} / {totalPages}
						</span>
						<Button
							variant="ghost"
							size="icon-sm"
							disabled={!hasNext}
							onclick={() => (page = page + 1)}
						>
							<ChevronRight class="size-4" />
						</Button>
					</div>
				</div>
			{:else if total > 0}
				<p class="mt-4 text-center text-xs text-muted-foreground">
					{total.toLocaleString()} {total === 1 ? 'entry' : 'entries'}
				</p>
			{/if}
		{/if}
	</div>
</div>
