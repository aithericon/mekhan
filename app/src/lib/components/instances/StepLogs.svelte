<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import { listProcessesByInstance, getProcessLogsTail } from '$lib/api/client';
	import type { components } from '$lib/api/schema';

	// The OpenAPI-derived shape that comes back from
	// `/api/v1/processes/{id}/logs/tail`.
	type LogRow = components['schemas']['LogRow'];

	type Props = {
		/** Owning workflow instance id. Logs are fetched across every process
		 *  linked to this instance (Loop / SubWorkflow can spawn extras). */
		instanceId?: string;
		/** Executor execution id for the step — one job per (node, iteration).
		 *  When present the tail query filters server-side to exactly this
		 *  execution. When absent we fall back to the step's time window, which
		 *  catches in-process backends that don't stamp `execution_id` (e.g. the
		 *  LLM backend whose tracing logs route through StreamContext). */
		executionId?: string | null;
		/** Step's ISO start time — lower bound of the fetch window. Required to
		 *  fetch; absent means the step hasn't started, so there's nothing to show. */
		startedAt?: string | null;
		/** Step's ISO completion time — upper bound. Open-ended while running. */
		completedAt?: string | null;
		/** Executor-reported total (from the output envelope's `detail.logs`),
		 *  used for the header count + a "(executor reported N)" footnote when the
		 *  fetched line count differs. Optional. */
		expectedCount?: number | null;
		/** Per-level counts from the envelope's `detail.logs.count_by_level`,
		 *  shown as badges in the header. Optional. */
		countByLevel?: Record<string, number> | null;
		/** Expand on mount (fetching immediately). Defaults to collapsed so a
		 *  click only costs the fetch when the user wants the lines. */
		defaultOpen?: boolean;
	};

	let {
		instanceId,
		executionId = null,
		startedAt,
		completedAt,
		expectedCount = null,
		countByLevel = null,
		defaultOpen = false
	}: Props = $props();

	let logsOpen = $state(false);
	let logRows = $state<LogRow[]>([]);
	let logsLoading = $state(false);
	let logsError = $state<string | null>(null);
	let logsFetched = $state(false);
	let expandedLogId = $state<number | null>(null);

	// Tiny pre/post buffer so the window catches the surrounding lifecycle
	// breadcrumbs the executor emits just before/after the step's projected
	// timestamps (executor `accepted` / `submitted` fire before `started_at`
	// was set on the row).
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
		if (!instanceId || !startedAt) return;
		logsLoading = true;
		logsError = null;
		try {
			const procs = await listProcessesByInstance(instanceId);
			const rows = (procs.items ?? []) as Array<{ process_id: string }>;
			if (rows.length === 0) {
				logRows = [];
				logsFetched = true;
				return;
			}
			const sinceTs = new Date(Date.parse(startedAt) - WINDOW_BUFFER_MS).toISOString();
			const untilTs = completedAt
				? new Date(Date.parse(completedAt) + WINDOW_BUFFER_MS).toISOString()
				: undefined;
			// Fetch logs across every process linked to this instance and
			// concatenate. In practice the instance has one process, but Loop /
			// SubWorkflow can spawn additional processes that legitimately log
			// during a single step. The `execution_id` filter (when known) is
			// applied server-side so we never client-trim a capped window.
			const tails = await Promise.all(
				rows.map((p) =>
					getProcessLogsTail(p.process_id, {
						since: sinceTs,
						until: untilTs,
						execution_id: executionId ?? undefined,
						limit: 500
					})
				)
			);
			const all = tails.flatMap((t) => t.logs);
			all.sort((a, b) => Date.parse(a.timestamp) - Date.parse(b.timestamp));
			// When we have an execution id the server already scoped the rows;
			// otherwise we keep the full time-window result.
			logRows = all;
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

	// Honour `defaultOpen` once, on mount: open + kick the fetch.
	$effect(() => {
		if (defaultOpen && !logsOpen && !logsFetched) {
			logsOpen = true;
			void fetchLogLines();
		}
	});

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
		// Prefer the structured `fields` sub-object, strip the noisy `petri_*`
		// routing keys the sidecar stamps on every log line.
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

	// Header count: prefer the executor-reported total before a fetch, then the
	// actual fetched line count once we have it.
	const headerCount = $derived<number | null>(
		logsFetched ? logRows.length : (expectedCount ?? null)
	);
</script>

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
		{#if headerCount !== null}
			<span class="ml-1 font-normal text-muted-foreground">
				({headerCount} entr{headerCount === 1 ? 'y' : 'ies'})
			</span>
		{/if}
		{#if countByLevel}
			<span class="ml-2 flex flex-wrap gap-1">
				{#each Object.entries(countByLevel) as [level, count] (level)}
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
					{#if expectedCount && expectedCount !== logRows.length}
						(executor reported {expectedCount})
					{/if}
				</p>
			{/if}
		</div>
	{/if}
</div>
