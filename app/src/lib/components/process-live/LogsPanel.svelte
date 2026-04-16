<script lang="ts">
	import { tick } from 'svelte';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { Badge } from '$lib/components/ui/badge';
	import ScrollText from '@lucide/svelte/icons/scroll-text';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import type { LogTailRow } from '$lib/api/client';
	import type { createProcessLiveStore } from '$lib/stores/process-live.svelte';

	type Store = ReturnType<typeof createProcessLiveStore>;
	interface Props {
		store: Store;
	}
	let { store }: Props = $props();

	let levelFilter = $state<string>('all');
	let searchInput = $state<string>('');
	let signalKeyInput = $state<string>('');
	let followTail = $state<boolean>(true);
	let expandedId = $state<number | null>(null);

	let scroller: HTMLDivElement | undefined = $state();

	const logLevelColors: Record<string, string> = {
		info: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		warn: 'bg-amber-100 text-amber-800 dark:bg-amber-900 dark:text-amber-200',
		error: 'bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200',
		debug: 'bg-slate-100 text-slate-600 dark:bg-slate-800 dark:text-slate-300'
	};

	function logLevelColor(l: string): string {
		return logLevelColors[l.toLowerCase()] ?? logLevelColors.debug;
	}

	function formatTimestamp(s: string): string {
		return new Intl.DateTimeFormat(undefined, {
			hour: '2-digit',
			minute: '2-digit',
			second: '2-digit'
		}).format(new Date(s));
	}

	function applyLevel(level: string) {
		levelFilter = level;
		store.setLogFilter({
			level: level === 'all' ? undefined : level,
			query: searchInput.trim() || undefined
		});
	}

	function applySearch() {
		store.setLogFilter({
			level: levelFilter === 'all' ? undefined : levelFilter,
			query: searchInput.trim() || undefined
		});
	}

	function applySignalKey() {
		store.setSignalKey(signalKeyInput.trim() || undefined);
	}

	function toggleExpand(id: number) {
		expandedId = expandedId === id ? null : id;
	}

	/**
	 * Structured fields live under `detail.fields` for executor-emitted logs
	 * (the sidecar auto-enriches kwargs there with execution_id, petri_net_id,
	 * petri_signal_key, plus user-supplied keys like iteration, phase, ...).
	 * Other log sources may put fields directly on `detail` — check both.
	 */
	function getFields(log: LogTailRow): Record<string, unknown> {
		const d = log.detail;
		if (!d || typeof d !== 'object') return {};
		const nested = (d as Record<string, unknown>).fields;
		if (nested && typeof nested === 'object') {
			return nested as Record<string, unknown>;
		}
		return d as Record<string, unknown>;
	}

	function field(log: LogTailRow, key: string): string | null {
		const v = getFields(log)[key];
		if (v === undefined || v === null || v === '') return null;
		return String(v);
	}

	/**
	 * Flatten + strip noisy routing keys for the expand view. The sidecar
	 * stamps ~15 `petri_*` entries on every log; they're useful once (to
	 * confirm routing context) but clutter the expand view. Keep the key
	 * identity fields prominent.
	 */
	const PROMOTED_KEYS = new Set(['iteration', 'source']);
	const NOISY_PREFIX = 'petri_event_'; // sig_artifact / sig_log / ... — redundant
	function rest(log: LogTailRow): Record<string, unknown> | null {
		const fields = getFields(log);
		const out: Record<string, unknown> = {};
		for (const [k, v] of Object.entries(fields)) {
			if (PROMOTED_KEYS.has(k)) continue;
			if (k.startsWith(NOISY_PREFIX)) continue;
			if (k.startsWith('petri_signal_') && k !== 'petri_signal_key') continue;
			out[k] = v;
		}
		return Object.keys(out).length > 0 ? out : null;
	}

	interface FoldedLog extends LogTailRow {
		repeat: number;
	}

	/**
	 * Collapse runs of identical (level, source, message) into a single row
	 * with a ×N counter. Rolls through the buffer in order; keeps the latest
	 * timestamp and detail so the row stays "fresh" as dupes stream in.
	 */
	const folded = $derived.by<FoldedLog[]>(() => {
		const out: FoldedLog[] = [];
		for (const l of store.logs) {
			const prev = out[out.length - 1];
			if (
				prev &&
				prev.level === l.level &&
				prev.source === l.source &&
				prev.message === l.message
			) {
				prev.repeat += 1;
				prev.timestamp = l.timestamp;
				prev.detail = l.detail;
			} else {
				out.push({ ...l, repeat: 1 });
			}
		}
		return out;
	});

	// Auto-scroll to bottom as new logs arrive, if follow-tail is on.
	$effect(() => {
		const _ = folded.length;
		if (!followTail || !scroller) return;
		tick().then(() => {
			if (scroller) scroller.scrollTop = scroller.scrollHeight;
		});
	});

	const statusLabel = $derived(
		store.logStatus === 'streaming'
			? 'live'
			: store.logStatus === 'reconnecting'
				? 'reconnecting…'
				: store.logStatus === 'loading'
					? 'loading…'
					: store.logStatus
	);
	const statusDotClass = $derived(
		store.logStatus === 'streaming'
			? 'bg-green-500'
			: store.logStatus === 'error'
				? 'bg-red-500'
				: 'bg-amber-500'
	);
</script>

<div class="flex flex-col gap-3">
	<!-- Top row: status + level filters + follow-tail -->
	<div class="flex flex-wrap items-center gap-3">
		<div class="flex items-center gap-1 text-xs">
			<span class="inline-block size-2 rounded-full {statusDotClass}"></span>
			<span class="text-muted-foreground">{statusLabel}</span>
		</div>

		<div class="flex items-center gap-1">
			{#each ['all', 'info', 'warn', 'error'] as level}
				<Button
					variant={levelFilter === level ? 'default' : 'ghost'}
					size="sm"
					onclick={() => applyLevel(level)}
				>
					{level.charAt(0).toUpperCase() + level.slice(1)}
				</Button>
			{/each}
		</div>

		<label class="flex cursor-pointer items-center gap-1 text-xs text-muted-foreground">
			<input type="checkbox" bind:checked={followTail} class="size-3.5 accent-primary" />
			Follow tail
		</label>
	</div>

	<!-- Filters row -->
	<div class="flex flex-wrap items-center gap-2">
		<Input
			placeholder="search message…"
			class="h-8 w-64 text-xs"
			bind:value={searchInput}
			onkeydown={(e: KeyboardEvent) => e.key === 'Enter' && applySearch()}
		/>
		<Button size="sm" variant="outline" onclick={applySearch}>Search</Button>

		<Input
			placeholder="signal_key (optional)"
			class="h-8 w-52 text-xs"
			bind:value={signalKeyInput}
			onkeydown={(e: KeyboardEvent) => e.key === 'Enter' && applySignalKey()}
		/>
		<Button size="sm" variant="outline" onclick={applySignalKey}>Drill-down</Button>
	</div>

	<!-- Log scroller -->
	{#if folded.length === 0 && store.logStatus !== 'loading'}
		<div
			class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-12"
		>
			<ScrollText class="size-8 text-muted-foreground/40" />
			<p class="mt-2 text-sm text-muted-foreground">No logs in window</p>
		</div>
	{:else}
		<div
			bind:this={scroller}
			class="max-h-[60vh] overflow-y-auto rounded-lg border border-border bg-card"
		>
			<div class="divide-y divide-border">
				{#each folded as log (log.id)}
					{@const iter = field(log, 'iteration')}
					{@const isExpanded = expandedId === log.id}
					{@const extra = rest(log)}
					{@const hasDetail = !!log.detail && Object.keys(log.detail).length > 0}
					<button
						type="button"
						class="flex w-full items-start gap-2 px-4 py-1.5 text-left text-xs hover:bg-accent/30 focus:bg-accent/40 focus:outline-none"
						onclick={() => toggleExpand(log.id)}
					>
						<ChevronRight
							class="mt-0.5 size-3 shrink-0 text-muted-foreground/50 transition-transform {isExpanded
								? 'rotate-90'
								: ''}"
						/>
						<span class="shrink-0 pt-0.5 tabular-nums text-muted-foreground">
							{formatTimestamp(log.timestamp)}
						</span>
						<Badge class={logLevelColor(log.level)} variant="secondary">
							{log.level}
						</Badge>
						{#if log.source}
							<span class="shrink-0 pt-0.5 font-mono text-muted-foreground">{log.source}</span>
						{/if}
						{#if iter}
							<Badge variant="outline" class="h-5 px-1.5 text-[10px] font-normal">
								iter {iter}
							</Badge>
						{/if}
						<span class="break-all pt-0.5 text-foreground">{log.message}</span>
						{#if log.repeat > 1}
							<span
								class="ml-auto shrink-0 pt-0.5 tabular-nums text-[11px] font-medium text-muted-foreground/70"
							>
								×{log.repeat}
							</span>
						{/if}
					</button>
					{#if isExpanded && hasDetail}
						<div class="bg-muted/40 px-10 py-2 text-xs">
							<pre
								class="overflow-x-auto font-mono text-[11px] text-foreground">{JSON.stringify(
									extra ?? log.detail,
									null,
									2
								)}</pre>
						</div>
					{:else if isExpanded}
						<div class="bg-muted/40 px-10 py-2 text-xs text-muted-foreground">
							No structured detail.
						</div>
					{/if}
				{/each}
			</div>
		</div>
		<p class="text-xs text-muted-foreground">
			{folded.length} row{folded.length === 1 ? '' : 's'}
			{#if folded.length !== store.logs.length}
				· {store.logs.length} raw entries
			{/if}
		</p>
	{/if}

	{#if store.error}
		<p class="text-xs text-red-500">{store.error}</p>
	{/if}
</div>
