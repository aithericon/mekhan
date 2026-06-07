<script lang="ts">
	import {
		listInventory,
		getInventoryStats,
		type InventoryEntry,
		type InventoryStats
	} from '$lib/api/client';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { CopyButton } from '$lib/components/ui/copy-button';
	import { Separator } from '$lib/components/ui/separator';
	import * as Select from '$lib/components/ui/select';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import HardDrive from '@lucide/svelte/icons/hard-drive';
	import Server from '@lucide/svelte/icons/server';
	import Layers from '@lucide/svelte/icons/layers';
	import Search from '@lucide/svelte/icons/search';
	import Hash from '@lucide/svelte/icons/hash';
	import Star from '@lucide/svelte/icons/star';
	import ArrowUpDown from '@lucide/svelte/icons/arrow-up-down';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';

	// ── State ──────────────────────────────────────────────────────────────────
	let entries = $state<InventoryEntry[]>([]);
	let stats = $state<InventoryStats | null>(null);
	let total = $state(0);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let page = $state(0);
	let pageSize = $state(25);
	let totalPages = $state(0);
	let hasNext = $state(false);
	let hasPrevious = $state(false);

	// Filters
	let statusFilter = $state<string>('all');
	let serverFilter = $state<string>('all');
	let canonicalFilter = $state<string>('all'); // all | canonical | non-canonical
	let searchQuery = $state('');
	let sortField = $state('-updated_at');

	const sortOptions = [
		{ value: '-updated_at', label: 'Recently updated' },
		{ value: 'updated_at', label: 'Oldest updated' },
		{ value: '-last_seen', label: 'Recently seen' },
		{ value: 'path', label: 'Path A-Z' },
		{ value: '-path', label: 'Path Z-A' },
		{ value: 'file_server_id', label: 'Server A-Z' }
	];

	// One physical copy moves through these states (docs/32 §4): observed →
	// hash-verified against the legacy baseline → registered by-reference →
	// bytes copied → source deleted. mismatch/orphan_* are reconcile outcomes.
	const statusColors: Record<string, string> = {
		indexed: 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300',
		verified: 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		registered: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		copied: 'bg-cyan-100 text-cyan-800 dark:bg-cyan-900 dark:text-cyan-200',
		deleted: 'bg-gray-100 text-gray-600 dark:bg-gray-800 dark:text-gray-400',
		mismatch: 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200',
		orphan_disk: 'bg-amber-100 text-amber-800 dark:bg-amber-900 dark:text-amber-200',
		orphan_db: 'bg-orange-100 text-orange-800 dark:bg-orange-900 dark:text-orange-200'
	};

	function statusColor(status: string): string {
		return statusColors[status] ?? 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300';
	}

	const formatDate = (s: string | null | undefined) =>
		s
			? new Intl.DateTimeFormat(undefined, {
					year: 'numeric', month: 'short', day: 'numeric',
					hour: '2-digit', minute: '2-digit'
				}).format(new Date(s))
			: '—';

	function truncatePath(path: string, max = 64): string {
		return path.length <= max ? path : '…' + path.slice(-(max - 1));
	}

	// ── Data loading ───────────────────────────────────────────────────────────
	async function load(
		status: string, server: string, canonical: string,
		search: string, sort: string, pg: number, pgSize: number
	) {
		loading = true;
		error = null;
		try {
			const [listResult, statsResult] = await Promise.all([
				listInventory({
					status: status === 'all' ? undefined : status,
					file_server_id: server === 'all' ? undefined : server,
					is_canonical:
						canonical === 'all' ? undefined : canonical === 'canonical',
					search: search.trim() || undefined,
					sort,
					page: pg,
					page_size: pgSize
				}),
				getInventoryStats()
			]);
			entries = listResult.items;
			total = listResult.total;
			totalPages = listResult.total_pages;
			hasNext = listResult.has_next;
			hasPrevious = listResult.has_previous;
			stats = statsResult;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load inventory';
			entries = [];
			total = 0;
		} finally {
			loading = false;
		}
	}

	function resetPage() { page = 0; }

	// Debounce text input; other filters apply immediately.
	let debounceTimer: ReturnType<typeof setTimeout> | undefined;

	$effect(() => {
		const status = statusFilter;
		const server = serverFilter;
		const canonical = canonicalFilter;
		const search = searchQuery;
		const sort = sortField;
		const pg = page;
		const pgSize = pageSize;

		clearTimeout(debounceTimer);
		debounceTimer = setTimeout(() => {
			load(status, server, canonical, search, sort, pg, pgSize);
		}, 250);

		return () => clearTimeout(debounceTimer);
	});
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-6xl px-6 py-8 animate-rise">

		<!-- Header -->
		<div class="mb-6">
			<div class="flex items-center gap-2">
				<HardDrive class="size-6 text-muted-foreground" />
				<h1 class="text-2xl font-semibold tracking-tight text-foreground">File Inventory</h1>
			</div>
			<p class="mt-1 text-sm text-muted-foreground">
				Physical file copies tracked across file servers, linked to the
				content-addressed catalogue by hash
			</p>
		</div>

		<!-- Stats cards -->
		{#if stats}
			<div class="mb-6 grid grid-cols-1 gap-3 md:grid-cols-3">
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-muted-foreground">
						<Layers class="size-4" />
						<span class="text-sm font-medium uppercase tracking-wide">Total copies</span>
					</div>
					<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
						{stats.total.toLocaleString()}
					</p>
				</div>
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-muted-foreground">
						<ArrowUpDown class="size-4" />
						<span class="text-sm font-medium uppercase tracking-wide">By status</span>
					</div>
					<div class="mt-2 flex flex-wrap gap-1">
						{#each stats.by_status as s}
							<button
								class="inline-flex"
								onclick={() => { statusFilter = s.key; resetPage(); }}
								title="Filter by {s.key}"
							>
								<Badge class={statusColor(s.key)} variant="secondary">
									{s.key}: {s.count.toLocaleString()}
								</Badge>
							</button>
						{:else}
							<span class="text-sm text-muted-foreground">—</span>
						{/each}
					</div>
				</div>
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-muted-foreground">
						<Server class="size-4" />
						<span class="text-sm font-medium uppercase tracking-wide">By server</span>
					</div>
					<div class="mt-2 flex flex-wrap gap-x-3 gap-y-0.5">
						{#each stats.by_server as s}
							<button
								class="text-sm text-muted-foreground hover:text-foreground"
								onclick={() => { serverFilter = s.key; resetPage(); }}
								title="Filter by {s.key}"
							>
								{s.key}: <span class="font-semibold text-foreground">{s.count.toLocaleString()}</span>
							</button>
						{:else}
							<span class="text-sm text-muted-foreground">—</span>
						{/each}
					</div>
				</div>
			</div>
		{/if}

		<Separator class="mb-4" />

		<!-- Filters bar -->
		<div class="mb-4 flex flex-wrap items-center gap-2">
			<div class="relative min-w-[14rem] flex-1">
				<Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
				<Input
					type="text"
					placeholder="Search path or content hash…"
					class="h-8 pl-8 text-sm"
					bind:value={searchQuery}
					oninput={resetPage}
				/>
			</div>

			<Select.Root
				type="single"
				value={statusFilter}
				onValueChange={(v) => { statusFilter = v ?? 'all'; resetPage(); }}
			>
				<Select.Trigger class="h-8 w-40 text-sm">
					{statusFilter === 'all' ? 'All statuses' : statusFilter}
				</Select.Trigger>
				<Select.Content>
					<Select.Item value="all" label="All statuses" />
					{#each (stats?.by_status ?? []) as s}
						<Select.Item value={s.key} label={s.key} />
					{/each}
				</Select.Content>
			</Select.Root>

			<Select.Root
				type="single"
				value={serverFilter}
				onValueChange={(v) => { serverFilter = v ?? 'all'; resetPage(); }}
			>
				<Select.Trigger class="h-8 w-44 text-sm">
					{serverFilter === 'all' ? 'All servers' : serverFilter}
				</Select.Trigger>
				<Select.Content>
					<Select.Item value="all" label="All servers" />
					{#each (stats?.by_server ?? []) as s}
						<Select.Item value={s.key} label={s.key} />
					{/each}
				</Select.Content>
			</Select.Root>

			<Select.Root
				type="single"
				value={canonicalFilter}
				onValueChange={(v) => { canonicalFilter = v ?? 'all'; resetPage(); }}
			>
				<Select.Trigger class="h-8 w-40 text-sm">
					{canonicalFilter === 'all'
						? 'Any copy'
						: canonicalFilter === 'canonical'
							? 'Canonical only'
							: 'Non-canonical'}
				</Select.Trigger>
				<Select.Content>
					<Select.Item value="all" label="Any copy" />
					<Select.Item value="canonical" label="Canonical only" />
					<Select.Item value="non-canonical" label="Non-canonical" />
				</Select.Content>
			</Select.Root>

			<Select.Root
				type="single"
				value={sortField}
				onValueChange={(v) => { if (v) { sortField = v; resetPage(); } }}
			>
				<Select.Trigger class="h-8 w-44 text-sm">
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

		<!-- Error -->
		{#if error}
			<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-200">
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
				<HardDrive class="size-10 text-muted-foreground/40" />
				<p class="mt-3 text-sm text-muted-foreground">No inventory entries</p>
				<p class="text-sm text-muted-foreground">
					Physical copies are recorded when a crawl observes files on a file server
				</p>
			</div>

		<!-- Results -->
		{:else}
			<!-- Column header -->
			<div class="grid grid-cols-12 gap-3 px-4 pb-1.5 text-sm font-semibold uppercase tracking-wider text-muted-foreground">
				<span class="col-span-6">Path</span>
				<span class="col-span-2">Server</span>
				<span class="col-span-2">Status</span>
				<span class="col-span-2 text-right">Last seen</span>
			</div>

			<div class="space-y-1.5">
				{#each entries as entry (entry.id)}
					<div class="rounded-lg border border-border bg-card px-4 py-2.5 transition-colors hover:bg-accent/30">
						<div class="grid grid-cols-12 items-center gap-3">
							<!-- Path + content hash -->
							<div class="col-span-6 min-w-0">
								<div class="flex items-center gap-1.5">
									{#if entry.is_canonical}
										<Tooltip.Root>
											<Tooltip.Trigger>
												<Star class="size-3.5 shrink-0 fill-amber-400 text-amber-400" />
											</Tooltip.Trigger>
											<Tooltip.Content>Canonical copy</Tooltip.Content>
										</Tooltip.Root>
									{/if}
									<span class="truncate font-mono text-sm text-foreground" title={entry.path}>
										{truncatePath(entry.path)}
									</span>
								</div>
								{#if entry.content_hash}
									<div class="mt-0.5 flex items-center gap-1 text-sm text-muted-foreground">
										<Hash class="size-3" />
										<span class="font-mono">{entry.content_hash.slice(0, 16)}</span>
										<CopyButton text={entry.content_hash} title="Copy content hash" iconClass="w-3 h-3" />
									</div>
								{:else}
									<span class="mt-0.5 block text-sm italic text-muted-foreground">no hash yet</span>
								{/if}
							</div>

							<!-- Server -->
							<div class="col-span-2 min-w-0">
								<span class="truncate text-sm text-muted-foreground" title={entry.file_server_id}>
									{entry.file_server_id}
								</span>
							</div>

							<!-- Status -->
							<div class="col-span-2">
								<Badge class={statusColor(entry.status)} variant="secondary">
									{entry.status}
								</Badge>
								{#if entry.migration_target}
									<Tooltip.Root>
										<Tooltip.Trigger>
											<span class="ml-1 text-sm text-muted-foreground">→</span>
										</Tooltip.Trigger>
										<Tooltip.Content>Migration target: {entry.migration_target}</Tooltip.Content>
									</Tooltip.Root>
								{/if}
							</div>

							<!-- Last seen -->
							<div class="col-span-2 text-right text-sm tabular-nums text-muted-foreground">
								{formatDate(entry.last_seen ?? entry.updated_at)}
							</div>
						</div>
					</div>
				{/each}
			</div>

			<!-- Pagination controls -->
			{#if totalPages > 1}
				<div class="mt-4 flex items-center justify-between">
					<p class="text-sm text-muted-foreground">
						Showing {entries.length} of {total.toLocaleString()} copies
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
						<span class="px-2 text-sm tabular-nums text-muted-foreground">
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
				<p class="mt-4 text-center text-sm text-muted-foreground">
					{total.toLocaleString()} {total === 1 ? 'copy' : 'copies'}
				</p>
			{/if}
		{/if}
	</div>
</div>
