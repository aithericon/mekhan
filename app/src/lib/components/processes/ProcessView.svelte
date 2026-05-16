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
	import type {
		ProcessTimelineEntry,
		StepDefinition,
		StepEvent,
		Phase,
		Progress
	} from '$lib/types/process';
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
	import Settings from '@lucide/svelte/icons/settings';

	let {
		processId,
		detail = $bindable<ProcessDetail | null>(null)
	}: { processId: string; detail?: ProcessDetail | null } = $props();

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
				human: false,
				started_at: p.started_at ?? undefined,
				completed_at: p.ended_at ?? undefined,
				duration_ms
			};
		});
	}

	function buildTimeline(d: ProcessDetail): ProcessTimelineEntry[] {
		// Canonical executor-domain model first; legacy Petri step_events fallback.
		const canonical = progressOf(d);
		if (canonical?.phases?.length) return phasesToTimeline(canonical.phases);

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
	let progress = $derived(progressOf(detail));
	let openTasks = $derived(
		detail ? detail.tasks.filter((t) => t.status === 'pending') : []
	);
	let progressPct = $derived(
		progress ? Math.round(Math.min(1, Math.max(0, progress.fraction)) * 100) : 0
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

	// After an inline action on the Overview "Open tasks" card, drop the task
	// from the (detail-derived) open list so it doesn't linger as pending.
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
			</Button>
		{/each}
	</div>

	<!-- ── Overview Tab ───────────────────────────────────────── -->
	{#if activeTab === 'overview'}
		<div class="space-y-4">
			{#if progress}
				<div class="rounded-lg border border-border bg-card p-4">
					<div class="mb-2 flex items-baseline justify-between gap-3">
						<h3 class="text-sm font-medium text-foreground">Progress</h3>
						<span class="text-xs tabular-nums text-muted-foreground">
							{progressPct}%{#if progress.total_steps > 0}
								· step {progress.current_step}/{progress.total_steps}{/if}
						</span>
					</div>
					<div class="h-2 w-full overflow-hidden rounded-full bg-muted/50">
						<div
							class="h-full rounded-full bg-cyan-500 transition-all duration-300"
							style="width: {progressPct}%"
						></div>
					</div>
					{#if progress.message}
						<p class="mt-1.5 text-xs text-muted-foreground">{progress.message}</p>
					{/if}
				</div>
			{/if}

			{#if timelineEntries.length > 0}
				<div class="rounded-lg border border-border bg-card p-4">
					<h3 class="mb-3 text-sm font-medium text-foreground">Timeline</h3>
					<ProcessTimeline entries={timelineEntries} />
				</div>
			{/if}

			{#if openTasks.length > 0}
				<div class="rounded-lg border border-border bg-card p-4">
					<h3 class="mb-3 flex items-center gap-2 text-sm font-medium text-foreground">
						<ListChecks class="size-4 text-muted-foreground" />
						Open tasks
						<Badge variant="secondary">{openTasks.length}</Badge>
					</h3>
					<div class="space-y-2">
						{#each openTasks as task (task.id)}
							{@const anyTask = task as unknown as {
								task_id?: string;
								id: string;
								steps?: unknown[];
							}}
							{@const taskId = anyTask.task_id ?? anyTask.id}
							{@const hasSteps =
								Array.isArray(anyTask.steps) && anyTask.steps.length > 0}
							<div
								class="flex items-start justify-between gap-4 rounded-md border border-border/60 bg-background px-3 py-2"
							>
								<div class="min-w-0 flex-1">
									<div class="flex flex-wrap items-center gap-1.5">
										<span class="text-sm font-medium text-foreground">{task.title}</span>
										{#if task.assignee}
											<span class="text-xs text-muted-foreground">· {task.assignee}</span>
										{/if}
									</div>
									<p class="mt-0.5 text-xs text-muted-foreground">
										Created {relativeTime(task.created_at)}
									</p>
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
									{#if !hasSteps}
										<Button
											variant="ghost"
											size="sm"
											class="text-green-700 hover:text-green-800 hover:bg-green-100 dark:text-green-400 dark:hover:bg-green-900"
											onclick={async () => {
												await handleCompleteTask(taskId);
												dropOpenTask(taskId, 'completed');
											}}
										>
											<Check class="size-3.5 mr-1" />
											Complete
										</Button>
										<Button
											variant="ghost"
											size="sm"
											class="text-red-700 hover:text-red-800 hover:bg-red-100 dark:text-red-400 dark:hover:bg-red-900"
											onclick={async () => {
												await handleCancelTask(taskId);
												dropOpenTask(taskId, 'cancelled');
											}}
										>
											<X class="size-3.5 mr-1" />
											Cancel
										</Button>
									{/if}
								</div>
							</div>
						{/each}
					</div>
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
	{:else if activeTab === 'config'}
		<div class="rounded-lg border border-border bg-card p-4">
			<h3 class="mb-2 text-sm font-medium text-foreground">Configuration</h3>
			{#if detail.config && Object.keys(detail.config).length > 0}
				<pre
					class="overflow-x-auto rounded-md bg-muted px-3 py-2 text-xs text-foreground">{JSON.stringify(
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
