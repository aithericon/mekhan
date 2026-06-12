<script lang="ts">
	import {
		getProcess,
		getProcessMetricsSummary,
		getProcessLogs,
		getProcessTasks,
		getProcessArtifacts,
		listStepExecutions,
		cancelTask,
		type ProcessDetail,
		type HpiTask,
		type HpiMetricSummary,
		type HpiLog,
		type CatalogueEntry,
		type StepExecution,
		type WorkflowInstance
	} from '$lib/api/client';
	import type { ProcessTimelineEntry, Phase, Progress } from '$lib/types/process';
	import { page } from '$app/state';
	import { ProcessTimeline } from '$lib/components/process-timeline';
	import { ArtifactCard } from '$lib/components/catalogue';
	import { MetricsPanel, LogsPanel, ArtifactsPanel } from '$lib/components/process-live';
	import { isShowcaseEntry } from '$lib/components/process-live/renderers/registry';
	import { createProcessLiveStore } from '$lib/stores/process-live.svelte';
	import { SmartValue } from '$lib/components/instances/output-renderers';
	import { onDestroy, untrack } from 'svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import Activity from '@lucide/svelte/icons/activity';
	import FileBox from '@lucide/svelte/icons/file-box';
	import BarChart3 from '@lucide/svelte/icons/bar-chart-3';
	import ScrollText from '@lucide/svelte/icons/scroll-text';
	import ListChecks from '@lucide/svelte/icons/list-checks';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
	import LogIn from '@lucide/svelte/icons/log-in';
	import Flag from '@lucide/svelte/icons/flag';
	import X from '@lucide/svelte/icons/x';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import Settings from '@lucide/svelte/icons/settings';

	let {
		processId,
		detail = $bindable<ProcessDetail | null>(null),
		instance = null
	}: {
		processId: string;
		detail?: ProcessDetail | null;
		/** When set, the Overview tab also surfaces the workflow's Start inputs
		 *  and End results (sourced from this instance's step_executions
		 *  projection), so the run's I/O is visible alongside the HPI summary. */
		instance?: WorkflowInstance | null;
	} = $props();

	// ── State ──────────────────────────────────────────────────────────────────
	let loading = $state(true);
	let error = $state<string | null>(null);

	type Tab = 'overview' | 'artifacts' | 'metrics' | 'logs' | 'tasks' | 'config';
	let activeTab = $state<Tab>('overview');

	// Artifacts tab
	let artifacts = $state<CatalogueEntry[]>([]);
	let artifactsTotal = $state(0);
	let artifactsPage = $state(0);
	let artifactsTotalPages = $state(0);
	let artifactsHasNext = $state(false);
	let artifactsLoading = $state(false);
	let expandedArtifact = $state<string | null>(null);

	// Metrics tab
	let metricsSummary = $state<HpiMetricSummary[]>([]);
	let metricsLoading = $state(false);

	// Logs tab
	let logs = $state<HpiLog[]>([]);
	let logsTotal = $state(0);
	let logsPage = $state(0);
	let logsTotalPages = $state(0);
	let logsHasNext = $state(false);
	let logsLoading = $state(false);
	let logsLevelFilter = $state('all');

	// Tasks tab
	let tasks = $state<HpiTask[]>([]);
	let tasksLoading = $state(false);

	// Live store for Metrics + Logs + Artifacts tabs — lazily created on first
	// visit, torn down when the process changes or this component unmounts.
	let liveStore = $state<ReturnType<typeof createProcessLiveStore> | null>(null);

	// Step executions feed the Overview tab's Inputs (Start node outputs) and
	// Results (End node outputs) sections. Loaded only when an `instance` is
	// supplied; polled while the run is live.
	let stepExecutions = $state<StepExecution[]>([]);
	const instanceIsTerminal = $derived(
		instance
			? instance.status === 'completed' ||
				instance.status === 'failed' ||
				instance.status === 'cancelled'
			: false
	);
	const startSteps = $derived(
		stepExecutions
			.filter(
				(s) => s.node_kind === 'start' && s.outputs !== null && s.outputs !== undefined
			)
			.sort(
				(a, b) =>
					a.node_id.localeCompare(b.node_id) || a.iteration_index - b.iteration_index
			)
	);
	const endSteps = $derived(
		stepExecutions
			.filter(
				(s) =>
					(s.node_kind === 'end' || s.node_kind === 'failure') &&
					s.outputs !== null &&
					s.outputs !== undefined
			)
			.sort(
				(a, b) =>
					a.node_id.localeCompare(b.node_id) || a.iteration_index - b.iteration_index
			)
	);
	const showInputs = $derived(!!instance && startSteps.length > 0);
	// Renderable (showcase) artifacts — declared render hints + image/video/
	// audio — get a media card on the Overview tab; the live store streaming
	// them is already alive there for the Recent-activity logs.
	const showcaseCount = $derived(
		liveStore ? liveStore.artifacts.filter(isShowcaseEntry).length : 0
	);
	const showResults = $derived(
		!!instance &&
			(endSteps.length > 0 ||
				(instance.result !== null && instance.result !== undefined))
	);

	// ── Colours / formatters (body-only) ───────────────────────────────────────
	const taskStatusColors: Record<string, string> = {
		pending: 'border-amber-200 bg-amber-50 text-amber-700 dark:border-amber-800 dark:bg-amber-950 dark:text-amber-300',
		completed: 'border-emerald-200 bg-emerald-50 text-emerald-700 dark:border-emerald-800 dark:bg-emerald-950 dark:text-emerald-300',
		cancelled: 'border-slate-200 bg-slate-50 text-slate-600 dark:border-slate-700 dark:bg-slate-900 dark:text-slate-300',
		failed: 'border-red-200 bg-red-50 text-red-600 dark:border-red-800 dark:bg-red-950 dark:text-red-300'
	};
	const taskStatusLabels: Record<string, string> = {
		pending: 'Pending',
		completed: 'Completed',
		cancelled: 'Cancelled',
		failed: 'Rejected'
	};
	function taskStatusLabel(s: string): string {
		return taskStatusLabels[s.toLowerCase()] ?? s;
	}
	function taskDateText(t: {
		status: string;
		created_at: string;
		completed_at?: string | null;
	}): string {
		switch (t.status?.toLowerCase()) {
			case 'completed':
				return `Completed ${relativeTime(t.completed_at ?? t.created_at)}`;
			case 'cancelled':
				return `Cancelled ${relativeTime(t.completed_at ?? t.created_at)}`;
			case 'failed':
				return `Rejected ${relativeTime(t.completed_at ?? t.created_at)}`;
			default:
				return `Received ${relativeTime(t.created_at)}`;
		}
	}
	function taskStatusColor(s: string): string {
		return taskStatusColors[s.toLowerCase()] ?? taskStatusColors.pending;
	}

	function relativeTime(dateStr: string): string {
		const now = Date.now();
		const then = new Date(dateStr).getTime();
		const diff = now - then;
		if (diff < 60_000) return 'just now';
		if (diff < 3600_000) return `${Math.floor(diff / 60_000)}m ago`;
		if (diff < 86400_000) return `${Math.floor(diff / 3600_000)}h ago`;
		return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(
			new Date(dateStr)
		);
	}

	// ── Timeline ──────────────────────────────────────────────────────────────
	function progressOf(d: ProcessDetail | null): Progress | undefined {
		const cfg = d?.config as Record<string, unknown> | undefined;
		return cfg?.progress as Progress | undefined;
	}

	function phasesToTimeline(phases: Phase[]): ProcessTimelineEntry[] {
		return phases.map((p) => {
			let duration_ms: number | undefined;
			if (p.started_at && p.ended_at) {
				duration_ms = new Date(p.ended_at).getTime() - new Date(p.started_at).getTime();
			}
			return {
				step: p.name,
				label: p.name,
				status: p.status,
				started_at: p.started_at ?? undefined,
				completed_at: p.ended_at ?? undefined,
				duration_ms
			};
		});
	}

	function buildTimeline(d: ProcessDetail): ProcessTimelineEntry[] {
		// Single timeline source: the canonical executor-domain phase model,
		// stored verbatim at config.progress.phases by the mekhan ingest
		// projector (typed process_phase effect).
		const canonical = progressOf(d);
		if (canonical?.phases?.length) return phasesToTimeline(canonical.phases);
		return [];
	}

	let timelineEntries = $derived(detail ? buildTimeline(detail) : []);
	let progress = $derived(progressOf(detail));
	let openTasks = $derived(
		detail ? detail.tasks.filter((t) => t.status === 'pending') : []
	);
	let progressPct = $derived(
		progress ? Math.round(Math.min(1, Math.max(0, progress.fraction)) * 100) : 0
	);

	// Per-tab counts rendered as chips on the tab pills themselves (no
	// separate stats card) — tabs without a cheap count get no chip.
	const tabCounts = $derived<Partial<Record<Tab, number>>>(
		detail
			? {
					artifacts: detail.artifact_count,
					tasks: detail.tasks.length,
					metrics: detail.recent_metrics.length
				}
			: {}
	);

	// ── Data loading ───────────────────────────────────────────────────────────
	async function loadDetail() {
		loading = true;
		error = null;
		try {
			detail = await getProcess(processId);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load process';
		} finally {
			loading = false;
		}
	}

	async function loadArtifacts() {
		artifactsLoading = true;
		try {
			const res = await getProcessArtifacts(processId, { page: artifactsPage, page_size: 20 });
			artifacts = res.items;
			artifactsTotal = res.total;
			artifactsTotalPages = res.total_pages;
			artifactsHasNext = res.has_next;
		} catch {
			artifacts = [];
		} finally {
			artifactsLoading = false;
		}
	}

	async function loadMetrics() {
		metricsLoading = true;
		try {
			metricsSummary = await getProcessMetricsSummary(processId);
		} catch {
			metricsSummary = [];
		} finally {
			metricsLoading = false;
		}
	}

	async function loadLogs() {
		logsLoading = true;
		try {
			const res = await getProcessLogs(processId, {
				level: logsLevelFilter === 'all' ? undefined : logsLevelFilter,
				page: logsPage,
				page_size: 50
			});
			logs = res.items;
			logsTotal = res.total;
			logsTotalPages = res.total_pages;
			logsHasNext = res.has_next;
		} catch {
			logs = [];
		} finally {
			logsLoading = false;
		}
	}

	async function loadTasks() {
		tasksLoading = true;
		try {
			tasks = await getProcessTasks(processId);
		} catch {
			tasks = [];
		} finally {
			tasksLoading = false;
		}
	}

	async function loadStepExecutions(instanceId: string) {
		try {
			stepExecutions = await listStepExecutions(instanceId);
		} catch {
			stepExecutions = [];
		}
	}

	// After an inline action on the Overview "Open tasks" card, drop the task
	// from the (detail-derived) open list so it doesn't linger as pending.
	// Link to a task while remembering where we came from, so the task page's
	// back button can return here (the instance/process run) instead of the
	// global /tasks list.
	function taskHref(taskId: string): string {
		const from = page.url.pathname + page.url.search;
		return `/tasks/${taskId}?from=${encodeURIComponent(from)}`;
	}

	function dropOpenTask(id: string, status: string) {
		if (!detail) return;
		detail = {
			...detail,
			tasks: detail.tasks.map((t) => {
				const a = t as unknown as { task_id?: string; id: string };
				return (a.task_id ?? a.id) === id ? { ...t, status } : t;
			})
		};
	}

	async function handleCancelTask(taskId: string) {
		try {
			await cancelTask(taskId, 'Cancelled from UI');
			await loadTasks();
		} catch {
			/* ignore */
		}
	}

	// ── Effects ────────────────────────────────────────────────────────────────
	$effect(() => {
		processId; // subscribe
		loadDetail();
	});

	// Inputs/Results: fetch step executions when an instance is provided, and
	// poll until the run terminates so the projection (eventually consistent)
	// catches up while the user is watching.
	$effect(() => {
		const id = instance?.id;
		if (!id) {
			stepExecutions = [];
			return;
		}
		loadStepExecutions(id);
		if (instanceIsTerminal) return;
		const t = setInterval(() => loadStepExecutions(id), 2000);
		return () => clearInterval(t);
	});

	// One effect, tracking only tab + processId. Everything that reads/writes
	// liveStore is untracked so it can't retrigger this effect.
	let prevProcessId: string | null = null;
	$effect(() => {
		const tab = activeTab;
		const tid = processId;
		untrack(() => {
			if (prevProcessId !== null && prevProcessId !== tid && liveStore) {
				liveStore.destroy();
				liveStore = null;
			}
			prevProcessId = tid;

			if (tab === 'tasks') loadTasks();
			else if (
				tab === 'overview' ||
				tab === 'artifacts' ||
				tab === 'metrics' ||
				tab === 'logs'
			) {
				// Overview embeds LogsPanel for its "Recent activity" section so
				// the rendering matches the Logs tab — that means the live store
				// must be available on overview too.
				if (!liveStore) {
					const store = createProcessLiveStore(tid);
					liveStore = store;
					store.init();
				}
				if (tab === 'artifacts') loadArtifacts();
			}
		});
	});

	onDestroy(() => {
		liveStore?.destroy();
		liveStore = null;
	});

	// Reload logs when level filter / page changes
	$effect(() => {
		if (activeTab === 'logs') {
			logsLevelFilter;
			logsPage;
			loadLogs();
		}
	});

	// Reload artifacts when page changes
	$effect(() => {
		if (activeTab === 'artifacts') {
			artifactsPage;
			loadArtifacts();
		}
	});

	const tabs: { key: Tab; label: string; icon: typeof Activity }[] = [
		{ key: 'overview', label: 'Overview', icon: LayoutDashboard },
		{ key: 'artifacts', label: 'Artifacts', icon: FileBox },
		{ key: 'metrics', label: 'Metrics', icon: BarChart3 },
		{ key: 'logs', label: 'Logs', icon: ScrollText },
		{ key: 'tasks', label: 'Tasks', icon: ListChecks },
		{ key: 'config', label: 'Configuration', icon: Settings }
	];
</script>

{#if loading}
	<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
		Loading...
	</div>
{:else if error}
	<div
		class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-800 dark:bg-amber-950 dark:text-amber-200"
	>
		{error}
	</div>
{:else if detail}
	<!-- Tabs -->
	<div class="mb-4 flex flex-wrap gap-1">
		{#each tabs as tab}
			<Button
				variant={activeTab === tab.key ? 'default' : 'ghost'}
				size="sm"
				onclick={() => {
					activeTab = tab.key;
				}}
				class="gap-1.5"
			>
				<tab.icon class="size-3.5" />
				{tab.label}
				{#if tabCounts[tab.key] !== undefined}
					<!-- opacity-based so the chip reads on both the active
					     (primary) and ghost pill backgrounds -->
					<span class="tabular-nums opacity-60">{tabCounts[tab.key]}</span>
				{/if}
			</Button>
		{/each}
	</div>

	<!-- ── Overview Tab ───────────────────────────────────────── -->
	{#if activeTab === 'overview'}
		<!-- Two-column tiling: a 2/3 reading column (results → inputs → live
		     activity, answer-first) beside a 1/3 status rail (needs-attention
		     tasks → progress → timeline → tab counts). On narrow viewports
		     the rail stacks FIRST — attention items lead. -->
		<div class="grid grid-cols-1 items-start gap-4 lg:grid-cols-3">
			<div class="order-2 min-w-0 space-y-4 lg:order-1 lg:col-span-2">
				{#if showInputs || showResults}
					<!-- The run's I/O pair: inputs left, results right — reads
					     in → out. A lone card spans the full column; the pair
					     splits side-by-side only at xl, where each half is still
					     wide enough for SmartValue's field tables. -->
					<div class="grid grid-cols-1 items-start gap-4 xl:grid-cols-2">
						{#if showInputs}
							<!-- The workflow's Start envelope(s) — what was fed into this run.
							     One section per Start block; SmartValue picks ProcessTokenEnvelope
							     (matches on `_instance_id`) so business fields surface above the
							     `_*` metadata disclosure. -->
							<div
								class={[
									'min-w-0 rounded-lg border border-border bg-card p-4',
									!showResults && 'xl:col-span-2'
								]}
							>
								<h3 class="mb-3 flex items-center gap-2 text-sm font-semibold text-foreground">
									<LogIn class="size-4 text-muted-foreground" />
									Inputs
									{#if startSteps.length > 1}
										<Badge variant="secondary">{startSteps.length}</Badge>
									{/if}
								</h3>
								<div class="space-y-4">
									{#each startSteps as step (step.node_id + '-' + step.iteration_index)}
										{#if startSteps.length > 1}
											<div class="mb-1 flex flex-wrap items-center gap-1.5 text-sm">
												<span class="font-mono text-muted-foreground">from</span>
												<span class="font-mono text-foreground">{step.node_id}</span>
											</div>
										{/if}
										<SmartValue
											value={step.outputs}
											ctx={{
												position: 'output',
												nodeKind: step.node_kind,
												instanceId: instance?.id,
												stepStartedAt: step.started_at ?? undefined,
												stepCompletedAt: step.completed_at ?? undefined,
												hideProcessMetadata: true
											}}
										/>
									{/each}
								</div>
							</div>
						{/if}

						{#if showResults}
							<!-- Workflow terminal(s). Prefer rendering each End-node step's
							     output envelope directly — `EndTerminalEnvelope` matches the
							     `{exit_code, name, status, process_id, task_id}` shape and
							     leads with the declared result. Fall back to `instance.result`
							     when the step projection hasn't caught up yet but the instance
							     row already carries the structured result envelope. -->
							<div
								class={[
									'min-w-0 rounded-lg border border-border bg-card p-4',
									!showInputs && 'xl:col-span-2'
								]}
							>
								<h3 class="mb-3 flex items-center gap-2 text-sm font-semibold text-foreground">
									<Flag class="size-4 text-muted-foreground" />
									Results
									{#if endSteps.length > 1}
										<Badge variant="secondary">{endSteps.length}</Badge>
									{/if}
								</h3>
								<div class="space-y-4">
									{#if endSteps.length > 0}
										{#each endSteps as step (step.node_id + '-' + step.iteration_index)}
											{#if endSteps.length > 1}
												<div class="mb-1 flex flex-wrap items-center gap-1.5 text-sm">
													<span class="font-mono text-muted-foreground">from</span>
													<span class="font-mono text-foreground">{step.node_id}</span>
												</div>
											{/if}
											<SmartValue
												value={step.outputs}
												ctx={{
													position: 'output',
													nodeKind: step.node_kind,
													instanceId: instance?.id,
													stepStartedAt: step.started_at ?? undefined,
													stepCompletedAt: step.completed_at ?? undefined,
													hideProcessMetadata: true
												}}
											/>
										{/each}
									{:else}
										<SmartValue
											value={instance?.result}
											ctx={{
												position: 'output',
												instanceId: instance?.id,
												hideProcessMetadata: true
											}}
										/>
									{/if}
								</div>
							</div>
						{/if}
					</div>
				{/if}

				{#if liveStore && showcaseCount > 0}
					<!-- Renderable artifacts (videos, images, audio, declared viz
					     hints) — same grouped viewer + iteration scrubber as the
					     Artifacts tab, embedded so the run's media is visible
					     without leaving the overview. -->
					<div class="rounded-lg border border-border bg-card p-4">
						<h3 class="mb-3 flex items-center gap-2 text-sm font-semibold text-foreground">
							<FileBox class="size-4 text-muted-foreground" />
							Media
							<Badge variant="secondary">{showcaseCount}</Badge>
							<button
								class="ml-auto text-sm font-normal text-muted-foreground underline-offset-2 hover:text-foreground hover:underline"
								onclick={() => (activeTab = 'artifacts')}
							>
								All artifacts
							</button>
						</h3>
						<ArtifactsPanel store={liveStore} renderableOnly />
					</div>
				{/if}

				<!-- Recent activity uses the exact same LogsPanel as the Logs tab so
				     row shape, level badges, expand toggles, and follow-tail behave
				     identically — otherwise the two surfaces look like different
				     systems for the same data. -->
				{#if liveStore}
					<div class="rounded-lg border border-border bg-card p-4">
						<h3 class="mb-3 text-sm font-semibold text-foreground">Recent activity</h3>
						<LogsPanel store={liveStore} />
					</div>
				{/if}
			</div>

			<div class="order-1 min-w-0 space-y-4 lg:order-2">
				{#if openTasks.length > 0}
					<div class="rounded-lg border border-border bg-card p-4">
						<h3 class="mb-3 flex items-center gap-2 text-sm font-semibold text-foreground">
							<ListChecks class="size-4 text-muted-foreground" />
							Open tasks
							<Badge variant="warm">{openTasks.length}</Badge>
						</h3>
						<div class="space-y-2">
							{#each openTasks as task (task.id)}
								{@const anyTask = task as unknown as {
									task_id?: string;
									id: string;
									steps?: unknown[];
								}}
								{@const taskId = anyTask.task_id ?? anyTask.id}
								<div class="group/task relative">
									<a
										href={taskHref(taskId)}
										class="block rounded-lg border border-border/60 bg-background px-3 py-2 pr-12 transition hover:border-primary/40 hover:shadow-sm"
									>
										<div class="flex items-center gap-2">
											<span class="truncate text-sm font-medium text-foreground">{task.title}</span>
											<Badge
												variant="outline"
												class="shrink-0 rounded-full border-amber-200 bg-amber-50 text-amber-700"
											>
												Pending
											</Badge>
										</div>
										<div class="mt-0.5 flex flex-wrap items-center gap-x-1.5 gap-y-0.5 text-sm text-muted-foreground">
											<span>Received {relativeTime(task.created_at)}</span>
											{#if task.assignee}
												<span class="text-muted-foreground/60">·</span>
												<span>{task.assignee}</span>
											{/if}
										</div>
									</a>
									<Button
										variant="ghost"
										size="icon-sm"
										title="Cancel task"
										aria-label="Cancel task"
										class="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:bg-red-100 hover:text-red-700 dark:hover:bg-red-900 dark:hover:text-red-400"
										onclick={async (e) => {
											e.preventDefault();
											e.stopPropagation();
											await handleCancelTask(taskId);
											dropOpenTask(taskId, 'cancelled');
										}}
									>
										<X class="size-4" />
									</Button>
								</div>
							{/each}
						</div>
					</div>
				{/if}

				{#if liveStore && detail.recent_metrics.length > 0}
					<!-- Compact live metrics — same store and renderer as the
					     Metrics tab (full controls live there). Only shown once
					     the process has actually emitted metrics. -->
					<div class="rounded-lg border border-border bg-card p-4">
						<h3 class="mb-2 flex items-center gap-2 text-sm font-semibold text-foreground">
							<BarChart3 class="size-4 text-muted-foreground" />
							Metrics
						</h3>
						<MetricsPanel {processId} store={liveStore} compact />
					</div>
				{/if}

				{#if progress}
					<div class="rounded-lg border border-border bg-card p-4">
						<div class="mb-2 flex items-baseline justify-between gap-3">
							<h3 class="text-sm font-semibold text-foreground">Progress</h3>
							<span class="text-sm tabular-nums text-muted-foreground">
								{progressPct}%{#if (progress.total_steps ?? 0) > 0}
									· step {progress.current_step ?? 0}/{progress.total_steps}{/if}
							</span>
						</div>
						<div class="h-2 w-full overflow-hidden rounded-full bg-muted/50">
							<div
								class="h-full rounded-full bg-cyan-500 transition-all duration-300"
								style="width: {progressPct}%"
							></div>
						</div>
						{#if progress.message}
							<p class="mt-1.5 text-sm text-muted-foreground">{progress.message}</p>
						{/if}
					</div>
				{/if}

				{#if timelineEntries.length > 0}
					<div class="rounded-lg border border-border bg-card p-4">
						<h3 class="mb-3 text-sm font-semibold text-foreground">Timeline</h3>
						<ProcessTimeline entries={timelineEntries} />
					</div>
				{/if}

			</div>
		</div>

		<!-- ── Artifacts Tab ──────────────────────────────────────── -->
	{:else if activeTab === 'artifacts'}
		{#if liveStore}
			<ArtifactsPanel store={liveStore} />
		{/if}
		{#if artifactsLoading}
			<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">
				Loading artifacts...
			</div>
		{:else if artifacts.length === 0}
			<div
				class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-12"
			>
				<FileBox class="size-8 text-muted-foreground/40" />
				<p class="mt-2 text-sm text-muted-foreground">No artifacts for this process</p>
			</div>
		{:else}
			<div class="space-y-2">
				{#each artifacts as entry (`${entry.execution_id}/${entry.id}`)}
					<ArtifactCard
						{entry}
						expanded={expandedArtifact === entry.id}
						highlighted={false}
						onToggle={() => {
							expandedArtifact = expandedArtifact === entry.id ? null : entry.id;
						}}
					/>
				{/each}
			</div>

			{#if artifactsTotalPages > 1}
				<div class="mt-4 flex items-center justify-between">
					<p class="text-sm text-muted-foreground">
						{artifactsTotal} artifact{artifactsTotal === 1 ? '' : 's'}
					</p>
					<div class="flex items-center gap-1">
						<Button
							variant="ghost"
							size="icon-sm"
							disabled={artifactsPage === 0}
							onclick={() => (artifactsPage = artifactsPage - 1)}
						>
							<ChevronLeft class="size-4" />
						</Button>
						<span class="px-2 text-sm tabular-nums text-muted-foreground">
							{artifactsPage + 1} / {artifactsTotalPages}
						</span>
						<Button
							variant="ghost"
							size="icon-sm"
							disabled={!artifactsHasNext}
							onclick={() => (artifactsPage = artifactsPage + 1)}
						>
							<ChevronRight class="size-4" />
						</Button>
					</div>
				</div>
			{/if}
		{/if}

		<!-- ── Metrics Tab ───────────────────────────────────────── -->
	{:else if activeTab === 'metrics'}
		{#if liveStore}
			<MetricsPanel {processId} store={liveStore} />
		{:else}
			<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">
				Loading metrics…
			</div>
		{/if}

		<!-- ── Logs Tab ──────────────────────────────────────────── -->
	{:else if activeTab === 'logs'}
		{#if liveStore}
			<LogsPanel store={liveStore} />
		{:else}
			<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">
				Loading logs…
			</div>
		{/if}

		<!-- ── Tasks Tab ─────────────────────────────────────────── -->
	{:else if activeTab === 'tasks'}
		{#if tasksLoading}
			<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">
				Loading tasks...
			</div>
		{:else if tasks.length === 0}
			<div
				class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-12"
			>
				<ListChecks class="size-8 text-muted-foreground/40" />
				<p class="mt-2 text-sm text-muted-foreground">No tasks for this process</p>
			</div>
		{:else}
			<div class="max-w-3xl space-y-2">
				{#each tasks as task (task.id)}
					{@const anyTask = task as unknown as {
						task_id?: string;
						id: string;
						steps?: unknown[];
					}}
					{@const taskId = anyTask.task_id ?? anyTask.id}
					<div class="group/task relative">
						<a
							href={taskHref(taskId)}
							class="block rounded-lg border border-border bg-card p-3 {task.status === 'pending' ? 'pr-12' : ''} transition hover:border-primary/40 hover:shadow-sm"
						>
							<div class="flex items-center gap-2">
								<span class="truncate text-sm font-medium text-foreground">{task.title}</span>
								<Badge variant="outline" class="shrink-0 rounded-full {taskStatusColor(task.status)}">
									{taskStatusLabel(task.status)}
								</Badge>
							</div>
							<div class="mt-0.5 flex flex-wrap items-center gap-x-1.5 gap-y-0.5 text-sm text-muted-foreground">
								<span>{taskDateText(task)}</span>
								{#if task.assignee}
									<span class="text-muted-foreground/60">·</span>
									<span>{task.assignee}</span>
								{/if}
							</div>
						</a>
						{#if task.status === 'pending'}
							<Button
								variant="ghost"
								size="icon-sm"
								title="Cancel task"
								aria-label="Cancel task"
								class="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:bg-red-100 hover:text-red-700 dark:hover:bg-red-900 dark:hover:text-red-400"
								onclick={(e) => {
									e.preventDefault();
									e.stopPropagation();
									handleCancelTask(taskId);
								}}
							>
								<X class="size-4" />
							</Button>
						{/if}
					</div>
				{/each}
			</div>
		{/if}
	{:else if activeTab === 'config'}
		<div class="rounded-lg border border-border bg-card p-4">
			<h3 class="mb-2 text-sm font-semibold text-foreground">Configuration</h3>
			{#if detail.config && Object.keys(detail.config).length > 0}
				<pre
					class="overflow-x-auto rounded-md bg-muted px-3 py-2 text-sm text-foreground">{JSON.stringify(
						detail.config,
						null,
						2
					)}</pre>
			{:else}
				<p class="text-sm text-muted-foreground">No configuration for this process.</p>
			{/if}
		</div>
	{/if}
{:else}
	<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
		Process not found
	</div>
{/if}
