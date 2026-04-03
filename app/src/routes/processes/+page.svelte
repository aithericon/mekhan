<script lang="ts">
	import { listProcesses, getProcessStats } from '$lib/api/client';
	import type { HpiProcess, ProcessStats } from '$lib/types/process';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Separator } from '$lib/components/ui/separator';
	import * as Select from '$lib/components/ui/select';
	import Search from '@lucide/svelte/icons/search';
	import Activity from '@lucide/svelte/icons/activity';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ArrowUpDown from '@lucide/svelte/icons/arrow-up-down';
	import CircleCheck from '@lucide/svelte/icons/circle-check';
	import CircleX from '@lucide/svelte/icons/circle-x';
	import Layers from '@lucide/svelte/icons/layers';
	import Zap from '@lucide/svelte/icons/zap';

	// ── State ──────────────────────────────────────────────────────────────────
	let processes = $state<HpiProcess[]>([]);
	let stats = $state<ProcessStats | null>(null);
	let total = $state(0);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let page = $state(0);
	let pageSize = $state(20);
	let totalPages = $state(0);
	let hasNext = $state(false);
	let hasPrevious = $state(false);

	// Filters
	let statusFilter = $state<string>('all');
	let searchQuery = $state('');
	let sortField = $state('-created_at');

	const sortOptions = [
		{ value: '-created_at', label: 'Newest first' },
		{ value: 'created_at', label: 'Oldest first' },
		{ value: '-updated_at', label: 'Recently updated' },
		{ value: 'updated_at', label: 'Least recently updated' }
	];

	// ── Status config ──────────────────────────────────────────────────────────
	const statusColors: Record<string, string> = {
		active: 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		completed: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		failed: 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200'
	};

	const kindColors: Record<string, string> = {
		'petri-net': 'bg-purple-100 text-purple-800 dark:bg-purple-900 dark:text-purple-200',
		'bo-campaign': 'bg-orange-100 text-orange-800 dark:bg-orange-900 dark:text-orange-200',
		pipeline: 'bg-cyan-100 text-cyan-800 dark:bg-cyan-900 dark:text-cyan-200'
	};

	function statusColor(status: string): string {
		return statusColors[status.toLowerCase()] ?? 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300';
	}

	function kindColor(kind: string): string {
		return kindColors[kind.toLowerCase()] ?? 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300';
	}

	// ── Helpers ────────────────────────────────────────────────────────────────
	function relativeTime(dateStr: string): string {
		const now = Date.now();
		const then = new Date(dateStr).getTime();
		const diff = now - then;
		if (diff < 60_000) return 'just now';
		if (diff < 3600_000) return `${Math.floor(diff / 60_000)}m ago`;
		if (diff < 86400_000) return `${Math.floor(diff / 3600_000)}h ago`;
		if (diff < 604800_000) return `${Math.floor(diff / 86400_000)}d ago`;
		return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(new Date(dateStr));
	}

	function displayName(p: HpiProcess): string {
		if (p.name) return p.name;
		return p.trace_id.length > 16 ? p.trace_id.slice(0, 16) + '...' : p.trace_id;
	}

	function resetPage() { page = 0; }

	// ── Data loading ───────────────────────────────────────────────────────────
	async function load(status: string, search: string, sort: string, pg: number, pgSize: number) {
		loading = true;
		error = null;
		try {
			const [listResult, statsResult] = await Promise.all([
				listProcesses({
					status: status === 'all' ? undefined : status,
					search: search.trim() || undefined,
					sort,
					page: pg,
					page_size: pgSize
				}),
				getProcessStats()
			]);
			processes = listResult.items;
			total = listResult.total;
			totalPages = listResult.total_pages;
			hasNext = listResult.has_next;
			hasPrevious = listResult.has_previous;
			stats = statsResult;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load processes';
			processes = [];
			total = 0;
		} finally {
			loading = false;
		}
	}

	// Debounce for text inputs
	let debounceTimer: ReturnType<typeof setTimeout> | undefined;

	$effect(() => {
		const status = statusFilter;
		const search = searchQuery;
		const sort = sortField;
		const pg = page;
		const pgSize = pageSize;

		clearTimeout(debounceTimer);
		debounceTimer = setTimeout(() => {
			load(status, search, sort, pg, pgSize);
		}, 300);

		return () => clearTimeout(debounceTimer);
	});
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">

		<!-- Header -->
		<div class="mb-6">
			<div class="flex items-center gap-2">
				<Activity class="size-6 text-muted-foreground" />
				<h1 class="text-2xl font-semibold tracking-tight text-foreground">Processes</h1>
			</div>
			<p class="mt-1 text-sm text-muted-foreground">
				Track and inspect running workflows, campaigns and pipelines
			</p>
		</div>

		<!-- Stats cards -->
		{#if stats}
			<div class="mb-6 grid grid-cols-4 gap-3">
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-muted-foreground">
						<Layers class="size-4" />
						<span class="text-xs font-medium uppercase tracking-wide">Total</span>
					</div>
					<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
						{stats.total.toLocaleString()}
					</p>
				</div>
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-green-600 dark:text-green-400">
						<Zap class="size-4" />
						<span class="text-xs font-medium uppercase tracking-wide">Active</span>
					</div>
					<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
						{stats.active.toLocaleString()}
					</p>
				</div>
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-blue-600 dark:text-blue-400">
						<CircleCheck class="size-4" />
						<span class="text-xs font-medium uppercase tracking-wide">Completed</span>
					</div>
					<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
						{stats.completed.toLocaleString()}
					</p>
				</div>
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-red-600 dark:text-red-400">
						<CircleX class="size-4" />
						<span class="text-xs font-medium uppercase tracking-wide">Failed</span>
					</div>
					<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
						{stats.failed.toLocaleString()}
					</p>
				</div>
			</div>
		{/if}

		<Separator class="mb-4" />

		<!-- Filters bar -->
		<div class="mb-4 space-y-3">
			<!-- Status tabs -->
			<div class="flex flex-wrap gap-1">
				{#each ['all', 'active', 'completed', 'failed'] as status}
					<Button
						variant={statusFilter === status ? 'default' : 'ghost'}
						size="sm"
						onclick={() => { statusFilter = status; resetPage(); }}
					>
						{status.charAt(0).toUpperCase() + status.slice(1)}
					</Button>
				{/each}
			</div>

			<!-- Search + sort -->
			<div class="flex items-center gap-2">
				<div class="relative flex-1">
					<Search class="absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-muted-foreground" />
					<Input
						type="text"
						placeholder="Search processes..."
						class="h-8 pl-8 text-sm"
						bind:value={searchQuery}
						oninput={resetPage}
					/>
				</div>

				<Select.Root
					type="single"
					value={sortField}
					onValueChange={(v) => { if (v) { sortField = v; resetPage(); } }}
				>
					<Select.Trigger class="h-8 w-48 text-sm">
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
			<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-800 dark:bg-amber-950 dark:text-amber-200">
				{error}
			</div>
		{/if}

		<!-- Loading -->
		{#if loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading...
			</div>

		<!-- Empty -->
		{:else if processes.length === 0}
			<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
				<Activity class="size-10 text-muted-foreground/40" />
				<p class="mt-3 text-sm text-muted-foreground">No processes found</p>
				<p class="text-xs text-muted-foreground">
					Processes appear here when workflows or campaigns are started
				</p>
			</div>

		<!-- Results -->
		{:else}
			<div class="space-y-2">
				{#each processes as process (process.trace_id)}
					<a
						href="/processes/{process.trace_id}"
						class="block rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/30"
					>
						<div class="flex items-start justify-between gap-4">
							<div class="min-w-0 flex-1">
								<div class="flex flex-wrap items-center gap-1.5">
									<span class="text-sm font-medium text-foreground truncate">
										{displayName(process)}
									</span>
									<Badge class={statusColor(process.status)} variant="secondary">
										{process.status}
									</Badge>
									{#if process.kind}
										<Badge class={kindColor(process.kind)} variant="secondary">
											{process.kind}
										</Badge>
									{/if}
								</div>

								<div class="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-muted-foreground">
									<span class="font-mono">{process.trace_id.slice(0, 12)}...</span>
									{#if process.owner}
										<span>Owner: {process.owner}</span>
									{/if}
									<span>Created {relativeTime(process.created_at)}</span>
									<span>Updated {relativeTime(process.updated_at)}</span>
								</div>
							</div>

							<ChevronRight class="size-4 shrink-0 text-muted-foreground mt-1" />
						</div>
					</a>
				{/each}
			</div>

			<!-- Pagination controls -->
			{#if totalPages > 1}
				<div class="mt-4 flex items-center justify-between">
					<p class="text-xs text-muted-foreground">
						Showing {processes.length} of {total.toLocaleString()} processes
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
					{total.toLocaleString()} {total === 1 ? 'process' : 'processes'}
				</p>
			{/if}
		{/if}
	</div>
</div>
