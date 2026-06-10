<script lang="ts">
	// Analytics tab orchestrator — Starfish-style capacity dashboard over the
	// file inventory. One shared scope bar (server / status), then independent
	// per-panel fetchers so a failing endpoint degrades ONE panel, not the tab:
	//   summary cards · directory treemap (app-controlled lazy drill via
	//   `under=`) · dimension breakdown · by-server donut · growth trend.
	import { listFileServers, type FileServersResponse } from '$lib/api/fileServers';
	import {
		getCatalogueStats,
		getInventoryStats,
		type CatalogueStats,
		type InventoryStats
	} from '$lib/api/client';
	import {
		getAnalyticsBreakdown,
		getAnalyticsTimeseries,
		triggerAnalyticsSnapshot,
		type BreakdownBucket,
		type BreakdownDimension,
		type BreakdownResponse,
		type SnapshotPoint
	} from '$lib/api/analytics';
	import {
		toTreemapNodes,
		pivotSnapshots,
		sizeClassLabel,
		sortSizeClasses,
		ageCohortLabel,
		topNWithOther,
		dedupRatio,
		GROWTH_WINDOWS
	} from './analytics-transforms';
	import { formatBytes, formatCount } from './format';
	import ChartPanel from './ChartPanel.svelte';
	import CapacityTreemap from './CapacityTreemap.svelte';
	import BreakdownChart from './BreakdownChart.svelte';
	import GrowthChart from './GrowthChart.svelte';
	import { FilterPills } from '$lib/components/shell';
	import { Button } from '$lib/components/ui/button';
	import * as Select from '$lib/components/ui/select';
	import { toast } from 'svelte-sonner';
	import HardDrive from '@lucide/svelte/icons/hard-drive';
	import FileBox from '@lucide/svelte/icons/file-box';
	import Server from '@lucide/svelte/icons/server';
	import Copy from '@lucide/svelte/icons/copy';
	import Camera from '@lucide/svelte/icons/camera';

	// ── Shared scope ─────────────────────────────────────────────────────────
	let serverScope = $state(''); // '' = all servers
	let statusScope = $state(''); // '' = all statuses

	function onServerScope(v: string | undefined) {
		serverScope = v ?? '';
		// A drill path only means something on the server it was descended on.
		dirPath = [];
	}

	// ── Summary cards ─────────────────────────────────────────────────────────
	// Total size / Files tracked are PHYSICAL inventory totals honoring the
	// scope filters (breakdown endpoint totals, not catalogue bytes — most
	// copies are inventory-only and never hashed into the catalogue). Dedup is
	// platform-wide: hashed physical bytes ÷ catalogue logical bytes.
	let invStats = $state<InventoryStats | null>(null);
	let catStats = $state<CatalogueStats | null>(null);
	let serversResp = $state<FileServersResponse | null>(null);
	let scopeTotals = $state<BreakdownResponse | null>(null);
	let hashedTotals = $state<BreakdownResponse | null>(null);
	let summaryError = $state<string | null>(null);

	async function loadSummary(server: string, status: string) {
		try {
			[invStats, catStats, serversResp, scopeTotals, hashedTotals] = await Promise.all([
				getInventoryStats(),
				getCatalogueStats(),
				listFileServers(),
				getAnalyticsBreakdown({
					group_by: 'server',
					limit: 1,
					file_server_id: server || undefined,
					status: status || undefined
				}),
				getAnalyticsBreakdown({ group_by: 'server', limit: 1, hashed: true })
			]);
			summaryError = null;
		} catch (e) {
			summaryError = e instanceof Error ? e.message : 'Failed to load summary stats';
		}
	}
	$effect(() => {
		loadSummary(serverScope, statusScope);
	});

	const serverCount = $derived(
		serversResp ? serversResp.servers.length + serversResp.unregistered.length : null
	);
	const dedup = $derived(
		catStats && hashedTotals ? dedupRatio(hashedTotals.total_bytes, catStats.total_size_bytes) : null
	);

	// ── Directory treemap (lazy drill: one level per fetch) ──────────────────
	// Breadcrumb segments; `under` = the joined prefix. Backend keys come back
	// relative to `under`, so a drill APPENDS the clicked key's segments.
	let dirPath = $state<string[]>([]);
	let treeBuckets = $state<BreakdownBucket[]>([]);
	let treeLoading = $state(true);
	let treeError = $state<string | null>(null);

	async function loadTreemap(server: string, status: string, path: string[]) {
		treeLoading = true;
		treeError = null;
		try {
			const resp = await getAnalyticsBreakdown({
				group_by: 'directory',
				under: path.length > 0 ? path.join('/') + '/' : undefined,
				depth: 1,
				limit: 200,
				file_server_id: server || undefined,
				status: status || undefined
			});
			treeBuckets = resp.buckets;
		} catch (e) {
			treeError = e instanceof Error ? e.message : 'Failed to load directory breakdown';
			treeBuckets = [];
		} finally {
			treeLoading = false;
		}
	}
	$effect(() => {
		const server = serverScope,
			status = statusScope,
			path = dirPath;
		loadTreemap(server, status, path);
	});

	const treeNodes = $derived(toTreemapNodes(treeBuckets));

	function onDrill(path: string) {
		dirPath = [...dirPath, ...path.split('/').filter(Boolean)];
	}

	// ── Dimension breakdown ──────────────────────────────────────────────────
	const DIMENSIONS: { value: BreakdownDimension; label: string }[] = [
		{ value: 'extension', label: 'Extension' },
		{ value: 'size_class', label: 'Size' },
		{ value: 'age', label: 'Age' },
		{ value: 'owner', label: 'Owner' }
	];
	let dimension = $state<BreakdownDimension>('extension');
	let breakdownMode = $state<'bytes' | 'count'>('bytes');
	let breakdownBuckets = $state<BreakdownBucket[]>([]);
	let breakdownLoading = $state(true);
	let breakdownError = $state<string | null>(null);

	async function loadBreakdown(dim: BreakdownDimension, server: string, status: string) {
		breakdownLoading = true;
		breakdownError = null;
		try {
			const resp = await getAnalyticsBreakdown({
				group_by: dim,
				file_server_id: server || undefined,
				status: status || undefined
			});
			breakdownBuckets = resp.buckets;
		} catch (e) {
			breakdownError = e instanceof Error ? e.message : 'Failed to load breakdown';
			breakdownBuckets = [];
		} finally {
			breakdownLoading = false;
		}
	}
	$effect(() => {
		const dim = dimension,
			server = serverScope,
			status = statusScope;
		loadBreakdown(dim, server, status);
	});

	// size_class has a natural smallest→largest order; the ranked dimensions
	// get top-N + "other" so a long tail can't unbound the bar list.
	const shapedBreakdown = $derived(
		dimension === 'size_class'
			? sortSizeClasses(breakdownBuckets)
			: topNWithOther(breakdownBuckets, 12, (b) => (breakdownMode === 'bytes' ? b.bytes : b.count))
	);
	const breakdownLabel = $derived(
		dimension === 'size_class' ? sizeClassLabel : dimension === 'age' ? ageCohortLabel : undefined
	);

	// ── By-server donut (status-scoped only — a server filter would reduce it
	// to a single slice) ─────────────────────────────────────────────────────
	let serverBuckets = $state<BreakdownBucket[]>([]);
	let serversLoading = $state(true);
	let serversError = $state<string | null>(null);

	async function loadServers(status: string) {
		serversLoading = true;
		serversError = null;
		try {
			const resp = await getAnalyticsBreakdown({
				group_by: 'server',
				status: status || undefined
			});
			serverBuckets = resp.buckets;
		} catch (e) {
			serversError = e instanceof Error ? e.message : 'Failed to load server breakdown';
			serverBuckets = [];
		} finally {
			serversLoading = false;
		}
	}
	$effect(() => {
		const status = statusScope;
		loadServers(status);
	});

	const shapedServers = $derived(topNWithOther(serverBuckets, 10));

	// ── Growth trend ─────────────────────────────────────────────────────────
	let growthWindow = $state(GROWTH_WINDOWS[1]); // default 7d
	let growthPoints = $state<SnapshotPoint[]>([]);
	let growthLoading = $state(true);
	let growthError = $state<string | null>(null);
	let growthRefresh = $state(0);
	let snapshotting = $state(false);

	async function loadGrowth(win: (typeof GROWTH_WINDOWS)[number], server: string) {
		growthLoading = true;
		growthError = null;
		try {
			growthPoints = await getAnalyticsTimeseries({
				dim: 'total',
				file_server_id: server || undefined,
				bucket_secs: win.bucketSecs,
				window_secs: win.windowSecs
			});
		} catch (e) {
			growthError = e instanceof Error ? e.message : 'Failed to load growth timeseries';
			growthPoints = [];
		} finally {
			growthLoading = false;
		}
	}
	$effect(() => {
		void growthRefresh;
		const win = growthWindow,
			server = serverScope;
		loadGrowth(win, server);
	});

	const growthPivot = $derived(pivotSnapshots(growthPoints, (p) => p.total_bytes));

	async function captureSnapshot() {
		snapshotting = true;
		try {
			const res = await triggerAnalyticsSnapshot();
			toast.success(`Snapshot captured — ${res.rows_written.toLocaleString()} rows`);
			growthRefresh += 1;
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'Snapshot failed');
		} finally {
			snapshotting = false;
		}
	}
</script>

<!-- Scope bar -->
<div class="mb-4 flex flex-wrap items-center gap-2">
	<Select.Root type="single" value={serverScope} onValueChange={onServerScope}>
		<Select.Trigger class="h-8 w-44 text-sm" data-testid="analytics-scope-server">
			{serverScope || 'All servers'}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="" label="All servers" />
			{#each serversResp?.servers ?? [] as s (s.key)}
				<Select.Item value={s.key} label={s.display_name || s.key} />
			{/each}
			{#each serversResp?.unregistered ?? [] as s (s.key)}
				<Select.Item value={s.key} label={s.key} />
			{/each}
		</Select.Content>
	</Select.Root>

	<Select.Root type="single" value={statusScope} onValueChange={(v) => (statusScope = v ?? '')}>
		<Select.Trigger class="h-8 w-40 text-sm" data-testid="analytics-scope-status">
			{statusScope || 'All statuses'}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="" label="All statuses" />
			{#each invStats?.by_status ?? [] as s (s.key)}
				<Select.Item value={s.key} label={s.key} />
			{/each}
		</Select.Content>
	</Select.Root>
</div>

{#if summaryError}
	<div
		class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-900 dark:bg-amber-950 dark:text-amber-200"
	>
		{summaryError}
	</div>
{/if}

<!-- Stat cards -->
<div class="mb-6 grid grid-cols-2 gap-3 lg:grid-cols-4" data-testid="analytics-summary">
	<div class="rounded-lg border border-border bg-card px-4 py-3">
		<div class="flex items-center gap-2 text-muted-foreground">
			<HardDrive class="size-4" />
			<span class="text-sm font-medium uppercase tracking-wide">Total size</span>
		</div>
		<p class="mt-1 text-2xl font-semibold text-foreground">
			{scopeTotals ? formatBytes(scopeTotals.total_bytes) : '—'}
		</p>
	</div>
	<div class="rounded-lg border border-border bg-card px-4 py-3">
		<div class="flex items-center gap-2 text-muted-foreground">
			<FileBox class="size-4" />
			<span class="text-sm font-medium uppercase tracking-wide">Files tracked</span>
		</div>
		<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
			{scopeTotals ? formatCount(scopeTotals.total_count) : '—'}
		</p>
	</div>
	<div class="rounded-lg border border-border bg-card px-4 py-3">
		<div class="flex items-center gap-2 text-muted-foreground">
			<Server class="size-4" />
			<span class="text-sm font-medium uppercase tracking-wide">Servers</span>
		</div>
		<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
			{serverCount !== null ? formatCount(serverCount) : '—'}
		</p>
	</div>
	<div class="rounded-lg border border-border bg-card px-4 py-3">
		<div class="flex items-center gap-2 text-muted-foreground">
			<Copy class="size-4" />
			<span class="text-sm font-medium uppercase tracking-wide">Dedup ratio</span>
		</div>
		<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
			{dedup !== null ? `${dedup.toFixed(2)}×` : '—'}
		</p>
	</div>
</div>

<div class="space-y-4">
	<!-- Directory treemap -->
	<ChartPanel
		title="Capacity by directory"
		subtitle="Click a tile to drill into it"
		loading={treeLoading}
		error={treeError}
		empty={treeNodes.length === 0}
		emptyMessage="No files under this prefix"
		testid="analytics-treemap"
	>
		{#snippet actions()}
			<nav class="flex flex-wrap items-center gap-1 text-sm" aria-label="Directory breadcrumb">
				<button
					type="button"
					class={dirPath.length === 0
						? 'font-medium text-foreground'
						: 'text-muted-foreground hover:text-foreground'}
					onclick={() => (dirPath = [])}
				>
					root
				</button>
				{#each dirPath as seg, i (i)}
					<span class="text-muted-foreground">›</span>
					<button
						type="button"
						class={i === dirPath.length - 1
							? 'font-medium text-foreground'
							: 'text-muted-foreground hover:text-foreground'}
						onclick={() => (dirPath = dirPath.slice(0, i + 1))}
					>
						{seg}
					</button>
				{/each}
			</nav>
		{/snippet}
		<CapacityTreemap nodes={treeNodes} {onDrill} />
	</ChartPanel>

	<div class="grid gap-4 lg:grid-cols-2">
		<!-- Dimension breakdown -->
		<ChartPanel
			title="Breakdown"
			loading={breakdownLoading}
			error={breakdownError}
			empty={shapedBreakdown.length === 0}
			emptyMessage="No files in scope"
			testid="analytics-breakdown"
		>
			{#snippet actions()}
				<FilterPills
					options={DIMENSIONS}
					active={dimension}
					onSelect={(v) => (dimension = v as BreakdownDimension)}
					testid="analytics-breakdown-dimension"
				/>
				<FilterPills
					options={[
						{ value: 'bytes', label: 'Bytes' },
						{ value: 'count', label: 'Files' }
					]}
					active={breakdownMode}
					onSelect={(v) => (breakdownMode = v as 'bytes' | 'count')}
					testid="analytics-breakdown-mode"
				/>
			{/snippet}
			<BreakdownChart buckets={shapedBreakdown} mode={breakdownMode} labelOf={breakdownLabel} />
		</ChartPanel>

		<!-- By-server donut -->
		<ChartPanel
			title="By server"
			subtitle="Physical bytes per file server"
			loading={serversLoading}
			error={serversError}
			empty={shapedServers.length === 0}
			emptyMessage="No inventory yet"
			testid="analytics-servers"
		>
			<BreakdownChart buckets={shapedServers} kind="donut" />
		</ChartPanel>
	</div>

	<!-- Growth trend -->
	<ChartPanel
		title="Growth"
		subtitle="Total bytes per server over snapshot history"
		loading={growthLoading}
		error={growthError}
		empty={growthPivot.categories.length === 0}
		emptyMessage="No snapshots in this window — capture one to start the series"
		testid="analytics-growth"
	>
		{#snippet actions()}
			<FilterPills
				options={GROWTH_WINDOWS.map((w) => ({ value: w.label, label: w.label }))}
				active={growthWindow.label}
				onSelect={(v) =>
					(growthWindow = GROWTH_WINDOWS.find((w) => w.label === v) ?? GROWTH_WINDOWS[1])}
				testid="analytics-growth-window"
			/>
			<Button
				variant="ghost"
				size="sm"
				disabled={snapshotting}
				onclick={captureSnapshot}
				data-testid="analytics-snapshot"
			>
				<Camera class="size-3.5" />
				{snapshotting ? 'Capturing…' : 'Capture snapshot'}
			</Button>
		{/snippet}
		<GrowthChart categories={growthPivot.categories} series={growthPivot.series} />
	</ChartPanel>
</div>
