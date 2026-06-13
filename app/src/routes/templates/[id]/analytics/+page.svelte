<script lang="ts">
	// Per-template analytics — structural shape + usage/duration/node rollups.
	// Each panel fetches independently so a failing endpoint degrades one panel,
	// not the whole tab (mirrors the AnalyticsTab.svelte pattern).
	//
	// Panels:
	//   stat-card grid  — total runs, success rate, failures, distinct users,
	//                     mean/p50/p95 duration, node count, subworkflow count,
	//                     version count, last run
	//   runs trend      — GrowthChart over timeseries (one series per outcome)
	//   node-kind mix   — BreakdownChart in count mode (structural breakdown)
	//   node hotspots   — slowest nodes + most-failing nodes (horizontal bar lists)
	import { page } from '$app/state';
	import {
		getTemplateAnalytics,
		getTemplateAnalyticsTimeseries,
		type TemplateAnalytics,
		type TemplateRunTimeseriesPoint
	} from '$lib/api/templates-analytics';
	import {
		pivotTemplateTimeseries,
		nodeKindBuckets,
		formatRunDuration,
		formatSuccessRate
	} from '$lib/components/templates/template-analytics-transforms';
	import ChartPanel from '$lib/components/data/ChartPanel.svelte';
	import GrowthChart from '$lib/components/data/GrowthChart.svelte';
	import BreakdownChart from '$lib/components/data/BreakdownChart.svelte';
	import { PageShell, FilterPills } from '$lib/components/shell';
	import { timeAgo } from '$lib/utils';

	import Activity from '@lucide/svelte/icons/activity';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import Users from '@lucide/svelte/icons/users';
	import Clock from '@lucide/svelte/icons/clock';
	import Timer from '@lucide/svelte/icons/timer';
	import Gauge from '@lucide/svelte/icons/gauge';
	import Workflow from '@lucide/svelte/icons/workflow';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import Layers from '@lucide/svelte/icons/layers';
	import CalendarClock from '@lucide/svelte/icons/calendar-clock';

	// ── Scope ─────────────────────────────────────────────────────────────────
	// Scope bar: which instance mode to include in the rollup-based panels.
	// `live` = real user runs; `test_run` = saved test cases; `draft` = draft runs.
	const MODES = [
		{ value: 'live', label: 'Live' },
		{ value: 'test_run', label: 'Test runs' },
		{ value: 'draft', label: 'Drafts' }
	];
	let mode = $state('live');

	const templateId = $derived(page.params.id!);

	// ── Summary + structural ──────────────────────────────────────────────────
	let analytics = $state<TemplateAnalytics | null>(null);
	let analyticsLoading = $state(true);
	let analyticsError = $state<string | null>(null);

	async function loadAnalytics(id: string, m: string) {
		analyticsLoading = true;
		analyticsError = null;
		try {
			analytics = await getTemplateAnalytics(id, { mode: m });
		} catch (e) {
			analyticsError = e instanceof Error ? e.message : 'Failed to load analytics';
			analytics = null;
		} finally {
			analyticsLoading = false;
		}
	}
	$effect(() => {
		const id = templateId;
		const m = mode;
		loadAnalytics(id, m);
	});

	// Derived display values — safely access nested fields.
	const usage = $derived(analytics?.usage ?? null);
	const duration = $derived(analytics?.duration ?? null);
	const structural = $derived(analytics?.structural ?? null);
	const hotspots = $derived(analytics?.node_hotspots ?? null);
	const versionCount = $derived(analytics?.version_count ?? null);

	const kindBuckets = $derived(
		structural?.node_kind_counts ? nodeKindBuckets(structural.node_kind_counts) : []
	);

	// ── Timeseries ────────────────────────────────────────────────────────────
	const TIMESERIES_WINDOWS = [
		{ label: '7d', windowSecs: 604_800, bucketSecs: 21_600 },
		{ label: '30d', windowSecs: 2_592_000, bucketSecs: 86_400 },
		{ label: '90d', windowSecs: 7_776_000, bucketSecs: 259_200 }
	];
	let tsWindow = $state(TIMESERIES_WINDOWS[1]);

	let timeseriesPoints = $state<TemplateRunTimeseriesPoint[]>([]);
	let timeseriesLoading = $state(true);
	let timeseriesError = $state<string | null>(null);

	async function loadTimeseries(
		id: string,
		m: string,
		win: (typeof TIMESERIES_WINDOWS)[number]
	) {
		timeseriesLoading = true;
		timeseriesError = null;
		try {
			timeseriesPoints = await getTemplateAnalyticsTimeseries(id, {
				mode: m,
				bucket_secs: win.bucketSecs,
				window_secs: win.windowSecs
			});
		} catch (e) {
			timeseriesError = e instanceof Error ? e.message : 'Failed to load run timeseries';
			timeseriesPoints = [];
		} finally {
			timeseriesLoading = false;
		}
	}
	$effect(() => {
		const id = templateId;
		const m = mode;
		const win = tsWindow;
		loadTimeseries(id, m, win);
	});

	const tsPivot = $derived(pivotTemplateTimeseries(timeseriesPoints));
</script>

<!-- PageShell width="default" creates the `h-full overflow-y-auto` scroll
     container + max-w-6xl body that conventions require. The layout's
     `flex-1 min-h-0` parent gives it a definite height to fill. -->
<PageShell width="default" testid="template-analytics-page">
	<div>
		<!-- Mode scope bar -->
		<div class="mb-5 flex items-center gap-3">
			<span class="text-sm text-muted-foreground">Mode</span>
			<FilterPills
				options={MODES}
				active={mode}
				onSelect={(v) => (mode = v)}
				testid="template-analytics-mode"
			/>
		</div>

		<!-- ── Stat cards ──────────────────────────────────────────────────── -->
		<div
			class="mb-6 grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5"
			data-testid="template-analytics-stats"
		>
			<!-- Total runs -->
			<div class="rounded-lg border border-border bg-card px-4 py-3">
				<div class="flex items-center gap-2 text-muted-foreground">
					<Activity class="size-4" />
					<span class="text-xs font-medium uppercase tracking-wide">Total runs</span>
				</div>
				<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
					{analyticsLoading ? '—' : (usage?.total_runs?.toLocaleString() ?? '—')}
				</p>
			</div>

			<!-- Success rate -->
			<div class="rounded-lg border border-border bg-card px-4 py-3">
				<div class="flex items-center gap-2 text-muted-foreground">
					<CheckCircle2 class="size-4" />
					<span class="text-xs font-medium uppercase tracking-wide">Success rate</span>
				</div>
				<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
					{analyticsLoading ? '—' : formatSuccessRate(usage?.success_rate)}
				</p>
			</div>

			<!-- Failures -->
			<div class="rounded-lg border border-border bg-card px-4 py-3">
				<div class="flex items-center gap-2 text-muted-foreground">
					<XCircle class="size-4" />
					<span class="text-xs font-medium uppercase tracking-wide">Failures</span>
				</div>
				<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
					{analyticsLoading
						? '—'
						: (usage?.runs_by_outcome?.failure?.toLocaleString() ?? '—')}
				</p>
			</div>

			<!-- Distinct users -->
			<div class="rounded-lg border border-border bg-card px-4 py-3">
				<div class="flex items-center gap-2 text-muted-foreground">
					<Users class="size-4" />
					<span class="text-xs font-medium uppercase tracking-wide">Users</span>
				</div>
				<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
					{analyticsLoading ? '—' : (usage?.distinct_users?.toLocaleString() ?? '—')}
				</p>
			</div>

			<!-- Mean duration -->
			<div class="rounded-lg border border-border bg-card px-4 py-3">
				<div class="flex items-center gap-2 text-muted-foreground">
					<Clock class="size-4" />
					<span class="text-xs font-medium uppercase tracking-wide">Mean duration</span>
				</div>
				<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
					{analyticsLoading ? '—' : formatRunDuration(usage?.mean_duration_ms)}
				</p>
			</div>

			<!-- p50 duration -->
			<div class="rounded-lg border border-border bg-card px-4 py-3">
				<div class="flex items-center gap-2 text-muted-foreground">
					<Timer class="size-4" />
					<span class="text-xs font-medium uppercase tracking-wide">p50</span>
				</div>
				<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
					{analyticsLoading ? '—' : formatRunDuration(duration?.p50_ms)}
				</p>
			</div>

			<!-- p95 duration -->
			<div class="rounded-lg border border-border bg-card px-4 py-3">
				<div class="flex items-center gap-2 text-muted-foreground">
					<Gauge class="size-4" />
					<span class="text-xs font-medium uppercase tracking-wide">p95</span>
				</div>
				<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
					{analyticsLoading ? '—' : formatRunDuration(duration?.p95_ms)}
				</p>
			</div>

			<!-- Node count (structural, mode-agnostic) -->
			<div class="rounded-lg border border-border bg-card px-4 py-3">
				<div class="flex items-center gap-2 text-muted-foreground">
					<Workflow class="size-4" />
					<span class="text-xs font-medium uppercase tracking-wide">Nodes</span>
				</div>
				<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
					{analyticsLoading ? '—' : (structural?.node_count?.toLocaleString() ?? '—')}
				</p>
			</div>

			<!-- Subworkflow count -->
			<div class="rounded-lg border border-border bg-card px-4 py-3">
				<div class="flex items-center gap-2 text-muted-foreground">
					<GitBranch class="size-4" />
					<span class="text-xs font-medium uppercase tracking-wide">Sub-workflows</span>
				</div>
				<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
					{analyticsLoading
						? '—'
						: (structural?.subworkflow_count?.toLocaleString() ?? '—')}
				</p>
			</div>

			<!-- Version count -->
			<div class="rounded-lg border border-border bg-card px-4 py-3">
				<div class="flex items-center gap-2 text-muted-foreground">
					<Layers class="size-4" />
					<span class="text-xs font-medium uppercase tracking-wide">Versions</span>
				</div>
				<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
					{analyticsLoading ? '—' : (versionCount?.toLocaleString() ?? '—')}
				</p>
			</div>

			<!-- Last run -->
			<div class="col-span-2 rounded-lg border border-border bg-card px-4 py-3 sm:col-span-1">
				<div class="flex items-center gap-2 text-muted-foreground">
					<CalendarClock class="size-4" />
					<span class="text-xs font-medium uppercase tracking-wide">Last run</span>
				</div>
				<p class="mt-1 text-lg font-semibold text-foreground" title={usage?.last_run ?? undefined}>
					{analyticsLoading
						? '—'
						: usage?.last_run
							? timeAgo(usage.last_run)
							: 'Never'}
				</p>
			</div>
		</div>

		<div class="space-y-4">
			<!-- ── Runs trend ─────────────────────────────────────────────────── -->
			<ChartPanel
				title="Runs over time"
				subtitle="Outcome breakdown across the version chain"
				loading={timeseriesLoading}
				error={timeseriesError}
				empty={tsPivot.categories.length === 0}
				emptyMessage="No run history in this window"
				testid="template-analytics-timeseries"
			>
				{#snippet actions()}
					<FilterPills
						options={TIMESERIES_WINDOWS.map((w) => ({ value: w.label, label: w.label }))}
						active={tsWindow.label}
						onSelect={(v) =>
							(tsWindow =
								TIMESERIES_WINDOWS.find((w) => w.label === v) ?? TIMESERIES_WINDOWS[1])}
						testid="template-analytics-ts-window"
					/>
				{/snippet}
				<GrowthChart
					categories={tsPivot.categories}
					series={tsPivot.series}
					mode="count"
					height={240}
				/>
			</ChartPanel>

			<div class="grid gap-4 lg:grid-cols-2">
				<!-- ── Node-kind mix ───────────────────────────────────────────── -->
				<ChartPanel
					title="Node types"
					subtitle="Structural composition of the latest published version"
					loading={analyticsLoading}
					error={analyticsError}
					empty={kindBuckets.length === 0}
					emptyMessage="No structural data — publish a version to compute metrics"
					testid="template-analytics-node-kinds"
				>
					<BreakdownChart buckets={kindBuckets} kind="donut" mode="count" height={240} />
				</ChartPanel>

				<!-- ── Most-failing nodes ──────────────────────────────────────── -->
				<ChartPanel
					title="Most-failing nodes"
					subtitle="Step executions with the highest failure count"
					loading={analyticsLoading}
					error={analyticsError}
					empty={!hotspots?.most_failing?.length}
					emptyMessage="No failure data yet"
					testid="template-analytics-failing-nodes"
				>
					<BreakdownChart
						buckets={(hotspots?.most_failing ?? []).map((h) => ({
							key: h.node_id,
							bytes: 0,
							count: h.failure_count
						}))}
						kind="bar"
						mode="count"
						height={240}
					/>
				</ChartPanel>
			</div>

			<!-- ── Slowest nodes ───────────────────────────────────────────────── -->
			<ChartPanel
				title="Slowest nodes"
				subtitle="Step executions ranked by mean wall-clock duration"
				loading={analyticsLoading}
				error={analyticsError}
				empty={!hotspots?.slowest?.length}
				emptyMessage="No timing data yet"
				testid="template-analytics-slowest-nodes"
			>
				<div class="divide-y divide-border">
					{#each hotspots?.slowest ?? [] as h (h.node_id)}
						<div class="flex items-center justify-between px-1 py-2 text-sm">
							<span class="truncate font-mono text-xs text-muted-foreground">{h.node_id}</span>
							<div class="ml-4 flex shrink-0 items-center gap-4 tabular-nums">
								<span class="text-foreground">
									{formatRunDuration(h.mean_duration_ms)}
								</span>
								<span class="text-muted-foreground" title="Total executions">
									{h.total_count.toLocaleString()} runs
								</span>
							</div>
						</div>
					{/each}
				</div>
			</ChartPanel>
		</div>
	</div>
</PageShell>
