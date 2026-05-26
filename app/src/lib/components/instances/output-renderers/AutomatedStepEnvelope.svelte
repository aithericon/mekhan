<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import AlertCircle from '@lucide/svelte/icons/alert-circle';
	import KeyValueList from './KeyValueList.svelte';
	import SmartValue from './SmartValue.svelte';
	import { listProcessesByInstance, getProcessLogsTail } from '$lib/api/client';
	import type { components } from '$lib/api/schema';
	import type { RendererProps } from './types';

	// The OpenAPI-derived shape that comes back from
	// `/api/v1/processes/{id}/logs/tail` (the `LogRow` re-export in
	// `$lib/api/client` is a hand-rolled near-duplicate with stricter optionality
	// — we use the schema-derived one so the response flows through unmunged).
	type LogRow = components['schemas']['LogRow'];

	// The executor envelope shape — see `executor/crates/executor-*/src/...` for
	// producers and `service/src/compiler/token_shape.rs`'s AutomatedStep arm
	// for the documented shape. `detail.outputs` carries the user-declared
	// business output; the rest is observability / lifecycle.
	type Phase = {
		name: string;
		status: string;
		started_at?: string;
		ended_at?: string;
	};

	type Envelope = {
		execution_id?: string;
		job_id?: string;
		run?: number;
		source?: string;
		status?: string;
		detail?: {
			outputs?: Record<string, unknown> | null;
			outcome?: { type?: string; [k: string]: unknown };
			duration_ms?: number;
			metrics?: {
				latest_values?: Record<string, unknown>;
				metric_names?: string[];
				total_points?: number;
			};
			progress?: {
				phases?: Phase[];
				message?: string;
				fraction?: number;
				updated_at?: string;
			};
			logs?: {
				count_by_level?: Record<string, number>;
				total_entries?: number;
			};
			stderr_tail?: string | null;
			stdout_tail?: string | null;
			artifact_manifest?: unknown;
		};
		[k: string]: unknown;
	};

	let { value, ctx }: RendererProps = $props();
	const env = $derived(value as Envelope);
	const detail = $derived(env.detail ?? {});

	const outcomeType = $derived<string | undefined>(
		typeof detail.outcome?.type === 'string' ? detail.outcome.type : undefined
	);
	const outcomeOk = $derived(outcomeType === 'success');
	const outcomeBad = $derived(outcomeType === 'failure' || outcomeType === 'error');

	const hasOutputs = $derived(
		!!detail.outputs &&
			typeof detail.outputs === 'object' &&
			Object.keys(detail.outputs).length > 0
	);

	const metrics = $derived(detail.metrics?.latest_values ?? null);
	const hasMetrics = $derived(!!metrics && Object.keys(metrics).length > 0);

	const phases = $derived<Phase[]>(detail.progress?.phases ?? []);
	const hasPhases = $derived(phases.length > 0);

	const hasStdout = $derived(typeof detail.stdout_tail === 'string' && detail.stdout_tail.length > 0);
	const hasStderr = $derived(typeof detail.stderr_tail === 'string' && detail.stderr_tail.length > 0);

	let logsOpen = $state(false);
	let stdioOpen = $state(false);

	// ── Live log lines for this execution ────────────────────────────────────
	// Logs land in `hpi_logs` keyed by `process_id`. Every line carries
	// `execution_id` in `detail.fields` via `event_emitter::enrich_log_fields`,
	// shared by the IPC sidecar (child-process SDK logs) and `StreamContext::log`
	// (in-process backends like the LLM). Fetch by step time window to avoid a
	// server-side filter dependency, then narrow strictly to rows stamped with
	// this execution_id — that's the single source of truth.
	let logRows = $state<LogRow[]>([]);
	let logsLoading = $state(false);
	let logsError = $state<string | null>(null);
	let logsFetched = $state(false);
	let expandedLogId = $state<number | null>(null);

	const stepStartedAt = $derived(ctx.stepStartedAt);
	const stepCompletedAt = $derived(ctx.stepCompletedAt);
	// Tiny pre/post buffer so the window catches the surrounding lifecycle
	// breadcrumbs the executor emits just before/after the step's
	// projected timestamps (executor `accepted` / `submitted` fire before
	// `started_at` was set on the row).
	const WINDOW_BUFFER_MS = 5_000;

	function rowExecutionId(row: LogRow): string | undefined {
		const d = row.detail;
		if (!d || typeof d !== 'object') return undefined;
		const o = d as Record<string, unknown>;
		const fields =
			o.fields && typeof o.fields === 'object' ? (o.fields as Record<string, unknown>) : o;
		const v = fields.execution_id;
		return typeof v === 'string' ? v : undefined;
	}

	async function fetchLogLines() {
		if (logsLoading) return;
		if (!ctx.instanceId || !stepStartedAt) return;
		logsLoading = true;
		logsError = null;
		try {
			const procs = await listProcessesByInstance(ctx.instanceId);
			const rows = (procs.items ?? []) as Array<{ process_id: string }>;
			if (rows.length === 0) {
				logRows = [];
				logsFetched = true;
				return;
			}
			const sinceTs = new Date(Date.parse(stepStartedAt) - WINDOW_BUFFER_MS).toISOString();
			const untilTs = stepCompletedAt
				? new Date(Date.parse(stepCompletedAt) + WINDOW_BUFFER_MS).toISOString()
				: undefined;
			// Fetch logs across every process linked to this instance and
			// concatenate. In practice the instance has one process, but Loop /
			// SubWorkflow can spawn additional processes that legitimately log
			// during a single step.
			const tails = await Promise.all(
				rows.map((p) =>
					getProcessLogsTail(p.process_id, {
						since: sinceTs,
						until: untilTs,
						limit: 500
					})
				)
			);
			const targetExecutionId = env.execution_id;
			const all = tails.flatMap((t) => t.logs);
			all.sort((a, b) => Date.parse(a.timestamp) - Date.parse(b.timestamp));
			logRows = targetExecutionId
				? all.filter((row) => rowExecutionId(row) === targetExecutionId)
				: all;
			logsFetched = true;
		} catch (e) {
			logsError = e instanceof Error ? e.message : String(e);
		} finally {
			logsLoading = false;
		}
	}

	function toggleLogs() {
		logsOpen = !logsOpen;
		if (logsOpen && !logsFetched) {
			void fetchLogLines();
		}
	}

	const logLevelColor: Record<string, string> = {
		info: 'bg-blue-100 text-blue-700',
		warn: 'bg-amber-100 text-amber-700',
		error: 'bg-red-100 text-red-700',
		debug: 'bg-slate-100 text-slate-600'
	};

	function levelColor(level: string): string {
		return logLevelColor[level.toLowerCase()] ?? logLevelColor.debug;
	}

	function formatTimeShort(iso: string): string {
		try {
			return new Intl.DateTimeFormat(undefined, {
				hour: '2-digit',
				minute: '2-digit',
				second: '2-digit'
			}).format(new Date(iso));
		} catch {
			return iso;
		}
	}

	function rowDetail(row: LogRow): Record<string, unknown> | null {
		const d = row.detail;
		if (!d || typeof d !== 'object') return null;
		// Mirror LogsPanel's logic: prefer the structured `fields` sub-object,
		// strip the noisy `petri_*` routing keys that the sidecar stamps on
		// every log line.
		const o = d as Record<string, unknown>;
		const fields = o.fields && typeof o.fields === 'object' ? (o.fields as Record<string, unknown>) : o;
		const out: Record<string, unknown> = {};
		for (const [k, v] of Object.entries(fields)) {
			if (k.startsWith('petri_event_')) continue;
			if (k.startsWith('petri_signal_') && k !== 'petri_signal_key') continue;
			out[k] = v;
		}
		return Object.keys(out).length > 0 ? out : null;
	}

	function toggleExpand(id: number) {
		expandedLogId = expandedLogId === id ? null : id;
	}

	function formatDuration(ms: number | undefined): string | null {
		if (ms === undefined || ms === null) return null;
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60_000) return `${(ms / 1000).toFixed(2)}s`;
		const m = Math.floor(ms / 60_000);
		const s = Math.floor((ms % 60_000) / 1000);
		return `${m}m ${s}s`;
	}

	function phaseDurationMs(p: Phase): number | null {
		if (!p.started_at || !p.ended_at) return null;
		const a = Date.parse(p.started_at);
		const b = Date.parse(p.ended_at);
		if (Number.isNaN(a) || Number.isNaN(b)) return null;
		return Math.max(0, b - a);
	}

	function formatTime(iso: string | undefined): string {
		if (!iso) return '—';
		try {
			return new Date(iso).toLocaleTimeString();
		} catch {
			return iso;
		}
	}
</script>

<div class="space-y-4">
	<!-- Outcome strip: the most important at-a-glance signal. -->
	<div class="flex flex-wrap items-center gap-2 text-sm">
		{#if outcomeOk}
			<Badge class="bg-green-100 text-green-700">
				<CheckCircle2 class="size-3.5" />
				<span class="ml-1">success</span>
			</Badge>
		{:else if outcomeBad}
			<Badge class="bg-red-100 text-red-700">
				<XCircle class="size-3.5" />
				<span class="ml-1">{outcomeType}</span>
			</Badge>
		{:else if outcomeType}
			<Badge variant="secondary" class="font-mono">{outcomeType}</Badge>
		{/if}
		{#if env.status && env.status !== outcomeType}
			<Badge variant="outline" class="font-mono">{env.status}</Badge>
		{/if}
		{#if detail.duration_ms !== undefined}
			<span class="text-muted-foreground">·</span>
			<span class="text-muted-foreground">{formatDuration(detail.duration_ms)}</span>
		{/if}
		{#if env.job_id}
			<span class="text-muted-foreground">·</span>
			<span class="text-muted-foreground">job</span>
			<code class="rounded bg-muted px-1.5 py-0.5 font-mono text-sm">{env.job_id}</code>
		{/if}
		{#if typeof env.run === 'number' && env.run > 0}
			<Badge variant="outline" class="font-mono">run {env.run}</Badge>
		{/if}
	</div>

	{#if hasOutputs}
		<!-- Cascade through SmartValue so shape-specific renderers (the
		     LLM response envelope, file refs, etc.) get a chance to
		     dispatch on `detail.outputs`. The outer container (drawer
		     "Outputs" section, or the "from <producer>" line in Inputs)
		     already provides the framing label. -->
		<SmartValue value={detail.outputs} {ctx} />
	{:else}
		<div class="text-sm text-muted-foreground italic">No business outputs.</div>
	{/if}

	{#if hasMetrics}
		<div>
			<div class="mb-1.5 text-sm font-semibold text-foreground">Metrics</div>
			<KeyValueList value={metrics} {ctx} />
		</div>
	{/if}

	{#if hasPhases}
		<div>
			<div class="mb-1.5 text-sm font-semibold text-foreground">Phases</div>
			<ol class="space-y-1 rounded-md border border-border bg-muted/20 p-2">
				{#each phases as p, i (i)}
					{@const dur = phaseDurationMs(p)}
					<li class="flex items-center gap-2 text-sm">
						{#if p.status === 'completed'}
							<CheckCircle2 class="size-3.5 shrink-0 text-green-600" />
						{:else if p.status === 'failed' || p.status === 'error'}
							<XCircle class="size-3.5 shrink-0 text-red-600" />
						{:else}
							<AlertCircle class="size-3.5 shrink-0 text-muted-foreground" />
						{/if}
						<span class="font-medium">{p.name}</span>
						<span class="ml-auto flex items-center gap-2 text-sm text-muted-foreground">
							{#if dur !== null}
								<span class="font-mono">{formatDuration(dur)}</span>
							{/if}
							<span class="font-mono">{formatTime(p.started_at)}</span>
						</span>
					</li>
				{/each}
			</ol>
		</div>
	{/if}

	{#if detail.logs && detail.logs.total_entries !== undefined && detail.logs.total_entries > 0}
		<div>
			<button
				type="button"
				class="flex w-full items-center gap-1 text-left text-sm font-semibold text-foreground hover:text-muted-foreground"
				onclick={toggleLogs}
			>
				{#if logsOpen}
					<ChevronDown class="size-3.5" />
				{:else}
					<ChevronRight class="size-3.5" />
				{/if}
				Logs
				<span class="ml-1 font-normal text-muted-foreground">
					({detail.logs.total_entries} entr{detail.logs.total_entries === 1 ? 'y' : 'ies'})
				</span>
				{#if detail.logs.count_by_level}
					<span class="ml-2 flex flex-wrap gap-1">
						{#each Object.entries(detail.logs.count_by_level) as [level, count] (level)}
							<Badge variant="outline" class="font-mono text-sm font-normal">
								{level}: {count}
							</Badge>
						{/each}
					</span>
				{/if}
			</button>

			{#if logsOpen}
				<div class="mt-2">
					{#if logsLoading && logRows.length === 0}
						<div class="rounded-md border border-border bg-muted/20 px-3 py-2 text-sm text-muted-foreground">
							Loading log lines…
						</div>
					{:else if logsError}
						<div class="flex items-center justify-between rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800">
							<span>{logsError}</span>
							<Button variant="ghost" size="sm" onclick={fetchLogLines}>Retry</Button>
						</div>
					{:else if logsFetched && logRows.length === 0}
						<div class="rounded-md border border-border bg-muted/20 px-3 py-2 text-sm text-muted-foreground italic">
							No log lines for this step.
						</div>
					{:else if logRows.length > 0}
						<div class="overflow-hidden rounded-md border border-border bg-card">
							<div class="max-h-80 divide-y divide-border overflow-y-auto">
								{#each logRows as row (row.id)}
									{@const isExpanded = expandedLogId === row.id}
									{@const extra = rowDetail(row)}
									<button
										type="button"
										class="flex w-full items-start gap-2 px-3 py-1 text-left text-sm hover:bg-accent/30 focus:bg-accent/40 focus:outline-none"
										onclick={() => toggleExpand(row.id)}
									>
										<ChevronRight
											class="mt-0.5 size-3 shrink-0 text-muted-foreground/50 transition-transform {isExpanded
												? 'rotate-90'
												: ''}"
										/>
										<span class="shrink-0 pt-0.5 tabular-nums font-mono text-sm text-muted-foreground">
											{formatTimeShort(row.timestamp)}
										</span>
										<Badge class={levelColor(row.level)} variant="secondary">
											{row.level}
										</Badge>
										{#if row.source}
											<span class="shrink-0 pt-0.5 font-mono text-sm text-muted-foreground">{row.source}</span>
										{/if}
										<span class="break-all pt-0.5 text-foreground">{row.message}</span>
									</button>
									{#if isExpanded && extra}
										<div class="bg-muted/40 px-9 py-2 text-sm">
											<pre class="overflow-x-auto font-mono text-sm whitespace-pre-wrap break-words text-foreground">{JSON.stringify(extra, null, 2)}</pre>
										</div>
									{:else if isExpanded}
										<div class="bg-muted/40 px-9 py-2 text-sm text-muted-foreground italic">
											No structured detail.
										</div>
									{/if}
								{/each}
							</div>
						</div>
						<p class="mt-1 text-sm text-muted-foreground">
							{logRows.length} line{logRows.length === 1 ? '' : 's'}
							{#if detail.logs.total_entries && detail.logs.total_entries !== logRows.length}
								(executor reported {detail.logs.total_entries})
							{/if}
						</p>
					{/if}
				</div>
			{/if}
		</div>
	{/if}

	{#if hasStdout || hasStderr}
		<div>
			<button
				type="button"
				class="flex items-center gap-1 text-sm font-semibold text-foreground hover:text-muted-foreground"
				onclick={() => (stdioOpen = !stdioOpen)}
			>
				{#if stdioOpen}
					<ChevronDown class="size-3.5" />
				{:else}
					<ChevronRight class="size-3.5" />
				{/if}
				Output streams
				{#if hasStderr}
					<Badge variant="outline" class="ml-1 font-mono">stderr</Badge>
				{/if}
				{#if hasStdout}
					<Badge variant="outline" class="font-mono">stdout</Badge>
				{/if}
			</button>
			{#if stdioOpen}
				<div class="mt-2 space-y-2">
					{#if hasStderr}
						<div>
							<div class="mb-1 text-sm font-mono text-muted-foreground">stderr (tail)</div>
							<pre class="rounded-md border border-border bg-muted/30 p-3 text-sm font-mono whitespace-pre-wrap break-words">{detail.stderr_tail}</pre>
						</div>
					{/if}
					{#if hasStdout}
						<div>
							<div class="mb-1 text-sm font-mono text-muted-foreground">stdout (tail)</div>
							<pre class="rounded-md border border-border bg-muted/30 p-3 text-sm font-mono whitespace-pre-wrap break-words">{detail.stdout_tail}</pre>
						</div>
					{/if}
				</div>
			{/if}
		</div>
	{/if}
</div>
