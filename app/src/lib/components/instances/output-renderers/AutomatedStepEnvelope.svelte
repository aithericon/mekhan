<script lang="ts">
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import AlertCircle from '@lucide/svelte/icons/alert-circle';
	import KeyValueList from './KeyValueList.svelte';
	import type { RendererProps } from './types';

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
		<div>
			<div class="mb-1.5 text-sm font-semibold text-foreground">Outputs</div>
			<KeyValueList value={detail.outputs} {ctx} />
		</div>
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
				class="flex items-center gap-1 text-sm font-semibold text-foreground hover:text-muted-foreground"
				onclick={() => (logsOpen = !logsOpen)}
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
			</button>
			{#if logsOpen && detail.logs.count_by_level}
				<div class="mt-1.5 flex flex-wrap gap-1.5">
					{#each Object.entries(detail.logs.count_by_level) as [level, count] (level)}
						<Badge variant="outline" class="font-mono">
							{level}: {count}
						</Badge>
					{/each}
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
