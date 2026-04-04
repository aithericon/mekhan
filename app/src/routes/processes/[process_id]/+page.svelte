<script lang="ts">
	import { page } from '$app/stores';
	import {
		getProcess,
		updateProcess,
		getProcessMetrics,
		getProcessLogs,
		getProcessTasks,
		getProcessArtifacts,
		completeTask,
		cancelTask
	} from '$lib/api/client';
	import type {
		ProcessDetail,
		HpiTask,
		HpiMetric,
		HpiLog,
		PaginatedProcessResponse
	} from '$lib/types/process';
	import type { CatalogueEntry } from '$lib/types/catalogue';
	import { ArtifactCard } from '$lib/components/catalogue';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Separator } from '$lib/components/ui/separator';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import Activity from '@lucide/svelte/icons/activity';
	import FileBox from '@lucide/svelte/icons/file-box';
	import BarChart3 from '@lucide/svelte/icons/bar-chart-3';
	import ScrollText from '@lucide/svelte/icons/scroll-text';
	import ListChecks from '@lucide/svelte/icons/list-checks';
	import LayoutDashboard from '@lucide/svelte/icons/layout-dashboard';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Check from '@lucide/svelte/icons/check';
	import X from '@lucide/svelte/icons/x';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';

	// ── State ──────────────────────────────────────────────────────────────────
	let detail = $state<ProcessDetail | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

	// Tab state
	type Tab = 'overview' | 'artifacts' | 'metrics' | 'logs' | 'tasks';
	let activeTab = $state<Tab>('overview');

	// Inline name editing
	let editingName = $state(false);
	let editNameValue = $state('');

	// Artifacts tab
	let artifacts = $state<CatalogueEntry[]>([]);
	let artifactsTotal = $state(0);
	let artifactsPage = $state(0);
	let artifactsTotalPages = $state(0);
	let artifactsHasNext = $state(false);
	let artifactsLoading = $state(false);
	let expandedArtifact = $state<string | null>(null);

	// Metrics tab
	let metrics = $state<HpiMetric[]>([]);
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

	const processId = $derived(($page.params as Record<string, string>).process_id);

	// ── Status / kind colours ──────────────────────────────────────────────────
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

	function statusColor(s: string): string {
		return statusColors[s.toLowerCase()] ?? 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300';
	}
	function kindColor(k: string): string {
		return kindColors[k.toLowerCase()] ?? 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300';
	}
	function logLevelColor(l: string): string {
		return logLevelColors[l.toLowerCase()] ?? logLevelColors.debug;
	}
	function taskStatusColor(s: string): string {
		return taskStatusColors[s.toLowerCase()] ?? taskStatusColors.pending;
	}

	// ── Helpers ────────────────────────────────────────────────────────────────
	const formatDate = (s: string) =>
		new Intl.DateTimeFormat(undefined, {
			year: 'numeric', month: 'short', day: 'numeric',
			hour: '2-digit', minute: '2-digit'
		}).format(new Date(s));

	function relativeTime(dateStr: string): string {
		const now = Date.now();
		const then = new Date(dateStr).getTime();
		const diff = now - then;
		if (diff < 60_000) return 'just now';
		if (diff < 3600_000) return `${Math.floor(diff / 60_000)}m ago`;
		if (diff < 86400_000) return `${Math.floor(diff / 3600_000)}h ago`;
		return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(new Date(dateStr));
	}

	function formatTimestamp(s: string): string {
		return new Intl.DateTimeFormat(undefined, {
			hour: '2-digit', minute: '2-digit', second: '2-digit'
		}).format(new Date(s));
	}

	// ── Metric aggregation ─────────────────────────────────────────────────────
	interface MetricSummary {
		key: string;
		latest: number;
		min: number;
		max: number;
		count: number;
	}

	function summarizeMetrics(all: HpiMetric[]): MetricSummary[] {
		const byKey = new Map<string, HpiMetric[]>();
		for (const m of all) {
			const arr = byKey.get(m.key) ?? [];
			arr.push(m);
			byKey.set(m.key, arr);
		}
		const summaries: MetricSummary[] = [];
		for (const [key, vals] of byKey) {
			const numbers = vals.map((v) => v.value);
			summaries.push({
				key,
				latest: numbers[0],
				min: Math.min(...numbers),
				max: Math.max(...numbers),
				count: numbers.length
			});
		}
		return summaries;
	}

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
			metrics = await getProcessMetrics(processId, { limit: 500 });
		} catch {
			metrics = [];
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

	// ── Actions ────────────────────────────────────────────────────────────────
	async function saveName() {
		if (!detail) return;
		try {
			await updateProcess(processId, { name: editNameValue });
			detail.name = editNameValue;
			editingName = false;
		} catch {
			// Silently fail — user can retry
		}
	}

	async function handleCompleteTask(taskId: string) {
		try {
			await completeTask(taskId, {});
			await loadTasks();
		} catch {
			// ignore
		}
	}

	async function handleCancelTask(taskId: string) {
		try {
			await cancelTask(taskId, 'Cancelled from UI');
			await loadTasks();
		} catch {
			// ignore
		}
	}

	// ── Effects ────────────────────────────────────────────────────────────────
	$effect(() => {
		processId; // subscribe
		loadDetail();
	});

	// Load tab data when switching tabs
	$effect(() => {
		const tab = activeTab;
		const _tid = processId;
		if (tab === 'artifacts') loadArtifacts();
		else if (tab === 'metrics') loadMetrics();
		else if (tab === 'logs') loadLogs();
		else if (tab === 'tasks') loadTasks();
	});

	// Reload logs when level filter changes
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

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">

		<!-- Back link -->
		<a
			href="/processes"
			class="mb-6 inline-flex items-center gap-1.5 text-sm text-muted-foreground transition-colors hover:text-foreground"
		>
			<ArrowLeft class="size-4" />
			Back to processes
		</a>

		{#if error}
			<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800 dark:border-amber-800 dark:bg-amber-950 dark:text-amber-200">
				{error}
			</div>
		{/if}

		{#if loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading...
			</div>

		{:else if detail}
			<!-- Header -->
			<div class="mb-6">
				<div class="flex items-center gap-2 mb-2">
					{#if editingName}
						<Input
							type="text"
							class="h-8 w-64 text-sm"
							bind:value={editNameValue}
							onkeydown={(e: KeyboardEvent) => { if (e.key === 'Enter') saveName(); if (e.key === 'Escape') editingName = false; }}
						/>
						<Button variant="ghost" size="icon-sm" onclick={saveName}>
							<Check class="size-4" />
						</Button>
						<Button variant="ghost" size="icon-sm" onclick={() => (editingName = false)}>
							<X class="size-4" />
						</Button>
					{:else}
						<h1 class="text-2xl font-semibold tracking-tight text-foreground">
							{detail.name ?? 'Unnamed Process'}
						</h1>
						<button
							class="text-muted-foreground hover:text-foreground transition-colors"
							onclick={() => { editNameValue = detail?.name ?? ''; editingName = true; }}
						>
							<Pencil class="size-4" />
						</button>
					{/if}
				</div>

				<div class="flex flex-wrap items-center gap-2 mb-2">
					<Badge class={statusColor(detail.status)} variant="secondary">
						{detail.status}
					</Badge>
					{#if detail.kind}
						<Badge class={kindColor(detail.kind)} variant="secondary">
							{detail.kind}
						</Badge>
					{/if}
					{#if detail.owner}
						<span class="text-xs text-muted-foreground">Owner: {detail.owner}</span>
					{/if}
				</div>

				<p class="font-mono text-xs text-muted-foreground mb-1">{detail.process_id}</p>
				<div class="flex items-center gap-4 text-xs text-muted-foreground">
					<span>Created {formatDate(detail.created_at)}</span>
					<span>Updated {relativeTime(detail.updated_at)}</span>
				</div>
			</div>

			<Separator class="mb-4" />

			<!-- Tabs -->
			<div class="mb-4 flex flex-wrap gap-1">
				{#each tabs as tab}
					<Button
						variant={activeTab === tab.key ? 'default' : 'ghost'}
						size="sm"
						onclick={() => { activeTab = tab.key; }}
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
					<!-- Quick stats -->
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

					<!-- Config -->
					{#if detail.config && Object.keys(detail.config).length > 0}
						<div class="rounded-lg border border-border bg-card p-4">
							<h3 class="mb-2 text-sm font-medium text-foreground">Configuration</h3>
							<pre class="overflow-x-auto rounded-md bg-muted px-3 py-2 text-xs text-foreground">{JSON.stringify(detail.config, null, 2)}</pre>
						</div>
					{/if}

					<!-- Recent logs -->
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
				{#if artifactsLoading}
					<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">
						Loading artifacts...
					</div>
				{:else if artifacts.length === 0}
					<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-12">
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
								onToggle={() => { expandedArtifact = expandedArtifact === entry.id ? null : entry.id; }}
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
				{#if metricsLoading}
					<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">
						Loading metrics...
					</div>
				{:else if metrics.length === 0}
					<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-12">
						<BarChart3 class="size-8 text-muted-foreground/40" />
						<p class="mt-2 text-sm text-muted-foreground">No metrics recorded</p>
					</div>
				{:else}
					{@const summaries = summarizeMetrics(metrics)}
					<div class="rounded-lg border border-border bg-card overflow-hidden">
						<table class="w-full text-sm">
							<thead>
								<tr class="border-b border-border bg-muted/50">
									<th class="px-4 py-2 text-left text-xs font-medium uppercase tracking-wide text-muted-foreground">Key</th>
									<th class="px-4 py-2 text-right text-xs font-medium uppercase tracking-wide text-muted-foreground">Latest</th>
									<th class="px-4 py-2 text-right text-xs font-medium uppercase tracking-wide text-muted-foreground">Min</th>
									<th class="px-4 py-2 text-right text-xs font-medium uppercase tracking-wide text-muted-foreground">Max</th>
									<th class="px-4 py-2 text-right text-xs font-medium uppercase tracking-wide text-muted-foreground">Count</th>
								</tr>
							</thead>
							<tbody>
								{#each summaries as s (s.key)}
									<tr class="border-b border-border last:border-0">
										<td class="px-4 py-2 font-mono text-foreground">{s.key}</td>
										<td class="px-4 py-2 text-right tabular-nums text-foreground font-medium">{s.latest.toPrecision(4)}</td>
										<td class="px-4 py-2 text-right tabular-nums text-muted-foreground">{s.min.toPrecision(4)}</td>
										<td class="px-4 py-2 text-right tabular-nums text-muted-foreground">{s.max.toPrecision(4)}</td>
										<td class="px-4 py-2 text-right tabular-nums text-muted-foreground">{s.count}</td>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				{/if}

			<!-- ── Logs Tab ──────────────────────────────────────────── -->
			{:else if activeTab === 'logs'}
				<!-- Level filter -->
				<div class="mb-3 flex gap-1">
					{#each ['all', 'info', 'warn', 'error'] as level}
						<Button
							variant={logsLevelFilter === level ? 'default' : 'ghost'}
							size="sm"
							onclick={() => { logsLevelFilter = level; logsPage = 0; }}
						>
							{level.charAt(0).toUpperCase() + level.slice(1)}
						</Button>
					{/each}
				</div>

				{#if logsLoading}
					<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">
						Loading logs...
					</div>
				{:else if logs.length === 0}
					<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-12">
						<ScrollText class="size-8 text-muted-foreground/40" />
						<p class="mt-2 text-sm text-muted-foreground">No logs found</p>
					</div>
				{:else}
					<div class="rounded-lg border border-border bg-card overflow-hidden">
						<div class="divide-y divide-border">
							{#each logs as log (log.id)}
								<div class="flex items-start gap-2 px-4 py-2 text-xs hover:bg-accent/30">
									<span class="shrink-0 tabular-nums text-muted-foreground pt-0.5">
										{formatTimestamp(log.timestamp)}
									</span>
									<Badge class={logLevelColor(log.level)} variant="secondary">
										{log.level}
									</Badge>
									{#if log.source}
										<span class="shrink-0 font-mono text-muted-foreground pt-0.5">{log.source}</span>
									{/if}
									<span class="text-foreground pt-0.5 break-all">{log.message}</span>
								</div>
							{/each}
						</div>
					</div>

					{#if logsTotalPages > 1}
						<div class="mt-4 flex items-center justify-between">
							<p class="text-xs text-muted-foreground">
								{logsTotal} log{logsTotal === 1 ? '' : 's'}
							</p>
							<div class="flex items-center gap-1">
								<Button
									variant="ghost"
									size="icon-sm"
									disabled={logsPage === 0}
									onclick={() => (logsPage = logsPage - 1)}
								>
									<ChevronLeft class="size-4" />
								</Button>
								<span class="px-2 text-xs tabular-nums text-muted-foreground">
									{logsPage + 1} / {logsTotalPages}
								</span>
								<Button
									variant="ghost"
									size="icon-sm"
									disabled={!logsHasNext}
									onclick={() => (logsPage = logsPage + 1)}
								>
									<ChevronRight class="size-4" />
								</Button>
							</div>
						</div>
					{/if}
				{/if}

			<!-- ── Tasks Tab ─────────────────────────────────────────── -->
			{:else if activeTab === 'tasks'}
				{#if tasksLoading}
					<div class="flex items-center justify-center py-12 text-sm text-muted-foreground">
						Loading tasks...
					</div>
				{:else if tasks.length === 0}
					<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-12">
						<ListChecks class="size-8 text-muted-foreground/40" />
						<p class="mt-2 text-sm text-muted-foreground">No tasks for this process</p>
					</div>
				{:else}
					<div class="space-y-2">
						{#each tasks as task (task.id)}
							<div class="rounded-lg border border-border bg-card p-4">
								<div class="flex items-start justify-between gap-4">
									<div class="min-w-0 flex-1">
										<div class="flex flex-wrap items-center gap-1.5">
											<span class="text-sm font-medium text-foreground">{task.title}</span>
											<Badge class={taskStatusColor(task.status)} variant="secondary">
												{task.status}
											</Badge>
										</div>
										<div class="mt-1 flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs text-muted-foreground">
											{#if task.assignee}
												<span>Assignee: {task.assignee}</span>
											{/if}
											<span>Created {relativeTime(task.created_at)}</span>
											{#if task.completed_at}
												<span>Completed {relativeTime(task.completed_at)}</span>
											{/if}
										</div>
									</div>

									{#if task.status === 'pending'}
										<div class="flex shrink-0 items-center gap-1">
											<Button
												variant="ghost"
												size="sm"
												class="text-green-700 hover:text-green-800 hover:bg-green-100 dark:text-green-400 dark:hover:bg-green-900"
												onclick={() => handleCompleteTask(task.id)}
											>
												<Check class="size-3.5 mr-1" />
												Complete
											</Button>
											<Button
												variant="ghost"
												size="sm"
												class="text-red-700 hover:text-red-800 hover:bg-red-100 dark:text-red-400 dark:hover:bg-red-900"
												onclick={() => handleCancelTask(task.id)}
											>
												<X class="size-3.5 mr-1" />
												Cancel
											</Button>
										</div>
									{/if}
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
	</div>
</div>
