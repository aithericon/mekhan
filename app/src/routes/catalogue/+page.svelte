<script lang="ts">
	import {
		listCatalogueEntries,
		getCatalogueStats
	} from '$lib/api/client';
	import type { CatalogueEntry, CatalogueStats } from '$lib/types/catalogue';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import Database from '@lucide/svelte/icons/database';
	import HardDrive from '@lucide/svelte/icons/hard-drive';
	import FileBox from '@lucide/svelte/icons/file-box';
	import Search from '@lucide/svelte/icons/search';
	import BarChart3 from '@lucide/svelte/icons/bar-chart-3';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';

	// ── State ──────────────────────────────────────────────────────────────────
	let entries = $state<CatalogueEntry[]>([]);
	let stats = $state<CatalogueStats | null>(null);
	let total = $state(0);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let activeCategory = $state<string>('all');
	let searchName = $state('');
	let sourceNetFilter = $state('');
	let expandedIds = $state<Set<string>>(new Set());
	let totalPages = $state(0);
	let hasNext = $state(false);

	// ── Category config ────────────────────────────────────────────────────────
	const categoryColors: Record<string, string> = {
		model:
			'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		dataset:
			'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		plot:
			'bg-purple-100 text-purple-800 dark:bg-purple-900 dark:text-purple-200',
		report:
			'bg-amber-100 text-amber-800 dark:bg-amber-900 dark:text-amber-200',
		artifact:
			'bg-slate-100 text-slate-800 dark:bg-slate-800 dark:text-slate-200',
		log: 'bg-gray-100 text-gray-700 dark:bg-gray-800 dark:text-gray-300'
	};

	const knownCategories = ['model', 'dataset', 'plot', 'report', 'artifact', 'log'];

	// ── Helpers ────────────────────────────────────────────────────────────────
	function formatBytes(bytes: number | null): string {
		if (bytes === null || bytes === undefined) return '—';
		if (bytes === 0) return '0 B';
		const units = ['B', 'KB', 'MB', 'GB', 'TB'];
		const i = Math.floor(Math.log(bytes) / Math.log(1024));
		const value = bytes / Math.pow(1024, i);
		return `${value.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
	}

	const formatDate = (s: string) =>
		new Intl.DateTimeFormat(undefined, {
			year: 'numeric',
			month: 'short',
			day: 'numeric',
			hour: '2-digit',
			minute: '2-digit'
		}).format(new Date(s));

	function truncatePath(path: string | null, max = 48): string {
		if (!path) return '—';
		if (path.length <= max) return path;
		return '…' + path.slice(-(max - 1));
	}

	function categoryColor(cat: string): string {
		return categoryColors[cat.toLowerCase()] ?? 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300';
	}

	function toggleExpanded(id: string) {
		const next = new Set(expandedIds);
		if (next.has(id)) {
			next.delete(id);
		} else {
			next.add(id);
		}
		expandedIds = next;
	}

	// ── Data loading ───────────────────────────────────────────────────────────
	async function load(category: string, name: string, sourceNet: string) {
		loading = true;
		error = null;
		try {
			const [listResult, statsResult] = await Promise.all([
				listCatalogueEntries({
					category: category === 'all' ? undefined : category,
					search: name.trim() || undefined,
					source_net: sourceNet.trim() || undefined,
					page: 0,
					page_size: 100
				}),
				getCatalogueStats()
			]);
			entries = listResult.items;
			total = listResult.total;
			totalPages = listResult.total_pages;
			hasNext = listResult.has_next;
			stats = statsResult;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load catalogue';
			entries = [];
			total = 0;
			totalPages = 0;
			hasNext = false;
		} finally {
			loading = false;
		}
	}

	// Debounce handle kept outside the effect so cleanup can cancel it
	let debounceTimer: ReturnType<typeof setTimeout> | undefined;

	$effect(() => {
		const cat = activeCategory;
		const name = searchName;
		const net = sourceNetFilter;

		clearTimeout(debounceTimer);
		debounceTimer = setTimeout(() => {
			load(cat, name, net);
		}, 300);

		return () => clearTimeout(debounceTimer);
	});
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
					<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
						{stats.by_category.length}
					</p>
				</div>
			</div>
		{/if}

		<!-- Filters bar -->
		<div class="mb-4 flex flex-wrap items-center gap-3">
			<!-- Category buttons -->
			<div class="flex flex-wrap gap-1">
				<Button
					variant={activeCategory === 'all' ? 'default' : 'ghost'}
					size="sm"
					onclick={() => (activeCategory = 'all')}
				>
					All
				</Button>
				{#each knownCategories as cat}
					<Button
						variant={activeCategory === cat ? 'default' : 'ghost'}
						size="sm"
						onclick={() => (activeCategory = cat)}
					>
						{cat.charAt(0).toUpperCase() + cat.slice(1)}
					</Button>
				{/each}
			</div>

			<!-- Search inputs -->
			<div class="ml-auto flex items-center gap-2">
				<div class="relative">
					<Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
					<Input
						type="text"
						placeholder="Search by name…"
						class="h-8 w-48 pl-8 text-sm"
						bind:value={searchName}
					/>
				</div>
				<Input
					type="text"
					placeholder="Source net…"
					class="h-8 w-36 text-sm"
					bind:value={sourceNetFilter}
				/>
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
				{#each entries as entry (entry.id)}
					{@const isExpanded = expandedIds.has(entry.id)}
					{@const hasMetadata =
						Object.keys(entry.file_metadata).length > 0 ||
						Object.keys(entry.user_metadata).length > 0}

					<div class="rounded-lg border border-border bg-card transition-colors hover:bg-accent/30">
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
								</div>

								<div class="mt-1.5 flex flex-wrap items-center gap-x-4 gap-y-0.5 text-xs text-muted-foreground">
									{#if entry.source_net}
										<span>Net: <span class="font-mono">{entry.source_net}</span></span>
									{/if}
									{#if entry.process_id}
										<span>Process: <span class="font-mono">{entry.process_id.slice(0, 8)}…</span></span>
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

							<div class="flex shrink-0 flex-col items-end gap-2">
								<span class="text-sm font-medium tabular-nums text-muted-foreground">
									{formatBytes(entry.size_bytes)}
								</span>
								{#if hasMetadata}
									<button
										class="flex items-center gap-1 text-[10px] text-muted-foreground transition-colors hover:text-foreground"
										onclick={() => toggleExpanded(entry.id)}
									>
										{#if isExpanded}
											<ChevronDown class="size-3" />
											Hide metadata
										{:else}
											<ChevronRight class="size-3" />
											Show metadata
										{/if}
									</button>
								{/if}
							</div>
						</div>

						<!-- Expanded metadata -->
						{#if isExpanded && hasMetadata}
							<div class="border-t border-border px-4 pb-4 pt-3">
								{#if Object.keys(entry.file_metadata).length > 0}
									<p class="mb-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
										File metadata
									</p>
									<pre class="overflow-x-auto rounded-md bg-muted px-3 py-2 text-[11px] text-foreground">{JSON.stringify(entry.file_metadata, null, 2)}</pre>
								{/if}
								{#if Object.keys(entry.user_metadata).length > 0}
									<p class="mb-1 mt-3 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
										User metadata
									</p>
									<pre class="overflow-x-auto rounded-md bg-muted px-3 py-2 text-[11px] text-foreground">{JSON.stringify(entry.user_metadata, null, 2)}</pre>
								{/if}
							</div>
						{/if}
					</div>
				{/each}
			</div>

			{#if total > entries.length}
				<p class="mt-4 text-center text-xs text-muted-foreground">
					Showing {entries.length} of {total.toLocaleString()} entries
					{#if hasNext}&nbsp;· {totalPages} page{totalPages !== 1 ? 's' : ''} total{/if}
				</p>
			{/if}
		{/if}
	</div>
</div>
