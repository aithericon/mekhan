<script lang="ts">
	import {
		getProcess,
		getProcessMetricsSummary,
		getProcessLogs,
		getProcessTasks,
		getProcessArtifacts,
		completeTask,
		cancelTask,
		type ProcessDetail,
		type HpiTask,
		type HpiMetricSummary,
		type HpiLog,
		type CatalogueEntry
	} from '$lib/api/client';
	import type { ProcessTimelineEntry, StepDefinition, StepEvent } from '$lib/types/process';
	import { ProcessTimeline } from '$lib/components/process-timeline';
	import { ArtifactCard } from '$lib/components/catalogue';
	import { MetricsPanel, LogsPanel, ArtifactsPanel } from '$lib/components/process-live';
	import { createProcessLiveStore } from '$lib/stores/process-live.svelte';
	import { onDestroy, untrack } from 'svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import Activity from '@lucide/svelte/icons/activity';
	import FileBox from '@lucide/svelte/icons/file-box';
	import BarChart3 from '@lucide/svelte/icons/bar-chart-3';
	import ScrollText from '@lucide/svelte/icons/scroll-text';
	import ListChecks from '@lucide/svelte/icons/list-checks';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
	import Check from '@lucide/svelte/icons/check';
	import X from '@lucide/svelte/icons/x';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ExternalLink from '@lucide/svelte/icons/external-link';

	let {
		processId,
		detail = $bindable<ProcessDetail | null>(null)
	}: { processId: string; detail?: ProcessDetail | null } = $props();

	// ── State ──────────────────────────────────────────────────────────────────
	let loading = $state(true);
	let error = $state<string | null>(null);

	type Tab = 'overview' | 'artifacts' | 'metrics' | 'logs' | 'tasks';
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

	// ── Colours / formatters (body-only) ───────────────────────────────────────
	const logLevelColors: Record<string, string> = {
		info: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		warn: 'bg-amber-100 text-amber-800 dark:bg-amber-900 dark:text-amber-200',
		error: 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200',
		debug: 'bg-slate-100 text-slate-600 dark:bg-slate-800 dark:text-slate-300'
	};
	const taskStatusColors: Record<string, string> = {
		pending: 'bg-amber-100 text-amber-800 dark:bg-amber-900 dark:text-amber-200',
		completed: 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		cancelled: 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300',
		failed: 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200'
	};
	function logLevelColor(l: string): string {
		return logLevelColors[l.toLowerCase()] ?? logLevelColors.debug;
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
	function formatTimestamp(s: string): string {
		return new Intl.DateTimeFormat(undefined, {
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		}).format(new Date(s));
	}

	// ── Timeline ──────────────────────────────────────────────────────────────
	function buildTimeline(d: ProcessDetail): ProcessTimelineEntry[] {
		const cfg = d.config as Record<string, unknown> | undefined;
		const dx = d as unknown as { steps?: StepDefinition[]; step_events?: StepEvent[] };
		const steps = (cfg?.steps ?? dx.steps) as StepDefinition[] | undefined;
		const events = (cfg?.step_events ?? dx.step_events ?? []) as StepEvent[];
		if (!steps?.length) return [];
		return steps.map((step) => {
			let firstStarted: string | undefined;
			let lastStarted: string | undefined;
			let lastCompleted: string | undefined;
			let startCount = 0;
			let completeCount = 0;
			for (const ev of events) {
				if (ev.started === step.key) {
					if (!firstStarted) firstStarted = ev.timestamp;
					lastStarted = ev.timestamp;
					startCount++;
				}
				if (ev.completed === step.key) {
					lastCompleted = ev.timestamp;
					completeCount++;
				}
			}

			let status: ProcessTimelineEntry['status'] = 'pending';
			if (lastCompleted && completeCount >= startCount) {
				status = 'completed';
			} else if (lastStarted) {
				status = 'running';
			}

			let duration_ms: number | undefined;
			if (firstStarted && lastCompleted) {
				duration_ms = new Date(lastCompleted).getTime() - new Date(firstStarted).getTime();
			}

			return {
				step: step.key,
				label: step.label,
				status,
				human: step.human,
				started_at: firstStarted,
				completed_at: lastCompleted,
				duration_ms,
				iterations: startCount > 1 ? startCount : undefined,
				completed_iterations: startCount > 1 ? completeCount : undefined
			};
		});
	}

	let timelineEntries = $derived(detail ? buildTimeline(detail) : []);

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

	async function handleCompleteTask(taskId: string) {
		try {
			await completeTask(taskId, {});
			await loadTasks();
		} catch {
			/* ignore */
		}
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
			else if (tab === 'artifacts' || tab === 'metrics' || tab === 'logs') {
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
		{ key: 'tasks', label: 'Tasks', icon: ListChecks }
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
			</Button>
		{/each}
	</div>

	<!-- ── Overview Tab ───────────────────────────────────────── -->
	{#if activeTab === 'overview'}
		<div class="space-y-4">
			{#if timelineEntries.length > 0}
				<div class="rounded-lg border border-border bg-card p-4">
					<h3 class="mb-3 text-sm font-medium text-foreground">Timeline</h3>
					<ProcessTimeline entries={timelineEntries} />
				</div>
			{/if}

			<div class="grid grid-cols-3 gap-3">
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-muted-foreground">
						<FileBox class="size-4" />
						<span class="text-xs font-medium uppercase tracking-wide">Artifacts</span>
					</div>
					<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
						{detail.artifact_count}
					</p>
				</div>
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-muted-foreground">
						<ListChecks class="size-4" />
						<span class="text-xs font-medium uppercase tracking-wide">Tasks</span>
					</div>
					<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
						{detail.tasks.length}
					</p>
				</div>
				<div class="rounded-lg border border-border bg-card px-4 py-3">
					<div class="flex items-center gap-2 text-muted-foreground">
						<BarChart3 class="size-4" />
						<span class="text-xs font-medium uppercase tracking-wide">Metrics</span>
					</div>
					<p class="mt-1 text-2xl font-semibold tabular-nums text-foreground">
						{detail.recent_metrics.length}
					</p>
				</div>
			</div>

			{#if detail.config && Object.keys(detail.config).length > 0}
				<div class="rounded-lg border border-border bg-card p-4">
					<h3 class="mb-2 text-sm font-medium text-foreground">Configuration</h3>
					<pre
						class="overflow-x-auto rounded-md bg-muted px-3 py-2 text-xs text-foreground">{JSON.stringify(
							detail.config,
							null,
							2
						)}</pre>
				</div>
			{/if}

			{#if detail.recent_logs.length > 0}
				<div class="rounded-lg border border-border bg-card p-4">
					<h3 class="mb-2 text-sm font-medium text-foreground">Recent Activity</h3>
					<div class="space-y-1">
						{#each detail.recent_logs.slice(0, 5) as log}
							<div class="flex items-start gap-2 text-xs">
								<span class="shrink-0 tabular-nums text-muted-foreground">
									{formatTimestamp(log.timestamp)}
								</span>
								<Badge class={logLevelColor(log.level)} variant="secondary">
									{log.level}
								</Badge>
								{#if log.source}
									<span class="shrink-0 font-mono text-muted-foreground">{log.source}</span>
								{/if}
								<span class="text-foreground">{log.message}</span>
							</div>
						{/each}
					</div>
				</div>
			{/if}
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
					<p class="text-xs text-muted-foreground">
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
						<span class="px-2 text-xs tabular-nums text-muted-foreground">
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
			<div class="space-y-2">
				{#each tasks as task (task.id)}
					{@const anyTask = task as unknown as {
						task_id?: string;
						id: string;
						steps?: unknown[];
					}}
					{@const taskId = anyTask.task_id ?? anyTask.id}
					{@const hasSteps = Array.isArray(anyTask.steps) && anyTask.steps.length > 0}
					<div class="rounded-lg border border-border bg-card p-4">
						<div class="flex items-start justify-between gap-4">
							<div class="min-w-0 flex-1">
								<div class="flex flex-wrap items-center gap-1.5">
									<span class="text-sm font-medium text-foreground">{task.title}</span>
									<Badge class={taskStatusColor(task.status)} variant="secondary">
										{task.status}
									</Badge>
								</div>
								<div
									class="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-muted-foreground"
								>
									{#if task.assignee}
										<span>Assignee: {task.assignee}</span>
									{/if}
									<span>Created {relativeTime(task.created_at)}</span>
									{#if task.completed_at}
										<span>Completed {relativeTime(task.completed_at)}</span>
									{/if}
								</div>
							</div>

							<div class="flex shrink-0 items-center gap-1">
								<Button
									variant="ghost"
									size="sm"
									href="/tasks/{taskId}"
									class="text-muted-foreground hover:text-foreground"
								>
									<ExternalLink class="size-3.5 mr-1" />
									Open
								</Button>
								{#if task.status === 'pending' && !hasSteps}
									<Button
										variant="ghost"
										size="sm"
										class="text-green-700 hover:text-green-800 hover:bg-green-100 dark:text-green-400 dark:hover:bg-green-900"
										onclick={() => handleCompleteTask(taskId)}
									>
										<Check class="size-3.5 mr-1" />
										Complete
									</Button>
									<Button
										variant="ghost"
										size="sm"
										class="text-red-700 hover:text-red-800 hover:bg-red-100 dark:text-red-400 dark:hover:bg-red-900"
										onclick={() => handleCancelTask(taskId)}
									>
										<X class="size-3.5 mr-1" />
										Cancel
									</Button>
								{/if}
							</div>
						</div>
					</div>
				{/each}
			</div>
		{/if}
	{/if}
{:else}
	<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
		Process not found
	</div>
{/if}
