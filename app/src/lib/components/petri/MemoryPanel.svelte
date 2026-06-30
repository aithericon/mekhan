<script lang="ts">
	import { RefreshCw, AlertTriangle } from '@lucide/svelte';
	import { displaySize } from '$lib/components/ui/file-drop-zone';
	import type { NetMemory } from '$lib/stores/petri-api';

	interface Props {
		memory: NetMemory | null;
		onRefresh?: () => void;
	}

	let { memory = null, onRefresh }: Props = $props();

	// Self-poll while this panel is mounted. Tabs are conditionally rendered in
	// NetWorkbench, so the panel only exists (and only polls) while the Memory
	// tab is open — no wasted requests when it's hidden. The engine endpoint is
	// hot-only, so polling never wakes a hibernated net.
	const POLL_MS = 3000;
	$effect(() => {
		if (!onRefresh) return;
		const id = setInterval(() => onRefresh(), POLL_MS);
		return () => clearInterval(id);
	});

	const store = $derived(memory?.store ?? null);

	// Share-of-total, for the little bars. Topology is folded in as its own row.
	const total = $derived(memory?.total_bytes ?? 0);
	function pct(bytes: number): number {
		return total > 0 ? Math.round((bytes / total) * 100) : 0;
	}

	// The dedup index is the historic OOM driver. Flag it when it dominates the
	// footprint AND has a non-trivial absolute size — the signature of the
	// streaming-telemetry dedup leak rather than normal one-shot idempotency.
	const dedupBytes = $derived(store?.base_dedup_bytes ?? 0);
	const dedupLeakSuspected = $derived(
		!!store && dedupBytes > 8 * 1024 * 1024 && pct(dedupBytes) >= 50
	);

	type Row = { label: string; bytes: number; hint?: string; warn?: boolean };
	const rows = $derived<Row[]>(
		store
			? [
					{
						label: 'Event tail',
						bytes: store.tail_bytes,
						hint: `${store.tail_events.toLocaleString()} events (bounded)`
					},
					{
						label: 'Base marking',
						bytes: store.base_marking_bytes,
						hint: 'parked / evicted tokens'
					},
					{
						label: 'Dedup index',
						bytes: store.base_dedup_bytes,
						hint: `${store.base_dedup_entries.toLocaleString()} entries (permanent)`,
						warn: dedupLeakSuspected
					},
					{ label: 'Topology', bytes: memory?.topology_bytes ?? 0, hint: 'net structure' }
				]
			: []
	);
</script>

<div class="flex h-full flex-col overflow-hidden">
	<div class="border-border bg-muted shrink-0 border-b px-3 py-2">
		<div class="flex items-center justify-between">
			<h3 class="text-foreground text-sm font-semibold">Memory</h3>
			<div class="flex items-center gap-2">
				{#if onRefresh}
					<button
						class="hover:bg-accent rounded p-1 transition-colors"
						onclick={onRefresh}
						aria-label="Refresh memory"
					>
						<RefreshCw class="text-muted-foreground h-3 w-3" />
					</button>
				{/if}
				{#if memory}
					<span
						class="bg-primary/15 text-primary rounded-full px-1.5 py-0.5 text-sm font-medium"
					>
						{displaySize(memory.total_bytes)}
					</span>
				{/if}
			</div>
		</div>
	</div>

	<div class="flex-1 overflow-y-auto p-3">
		{#if !memory}
			<p class="text-muted-foreground p-2 text-sm">Loading…</p>
		{:else if !memory.resident || !store}
			<div class="flex flex-col items-center justify-center py-8 text-center">
				<p class="text-muted-foreground text-sm">Net is hibernated</p>
				<p class="text-muted-foreground/70 mt-1 text-xs">
					No in-memory footprint — state lives on the durable log.
				</p>
			</div>
		{:else}
			<!-- Total -->
			<div class="mb-3">
				<div class="text-muted-foreground text-xs uppercase tracking-wide">Total resident</div>
				<div class="text-foreground font-mono text-2xl font-semibold">
					{displaySize(memory.total_bytes)}
				</div>
				<div class="text-muted-foreground text-xs">
					{store.event_count.toLocaleString()} events · {store.base_count.toLocaleString()} evicted
					to base
				</div>
			</div>

			{#if dedupLeakSuspected}
				<div
					class="border-warning/30 bg-warning/10 mb-3 flex items-start gap-2 rounded-md border p-2"
				>
					<AlertTriangle class="text-warning mt-0.5 h-4 w-4 shrink-0" />
					<div class="text-warning text-xs">
						The permanent dedup index dominates this net's footprint
						({pct(dedupBytes)}%). This is the signature of a streaming-telemetry
						dedup leak — check that high-volume emits aren't carrying dedup ids.
					</div>
				</div>
			{/if}

			<!-- Breakdown bars -->
			<div class="space-y-2.5">
				{#each rows as row (row.label)}
					<div>
						<div class="flex items-baseline justify-between text-sm">
							<span class="text-foreground flex items-center gap-1">
								{row.label}
								{#if row.warn}
									<AlertTriangle class="text-warning h-3 w-3" />
								{/if}
							</span>
							<span class="text-muted-foreground font-mono text-xs">
								{displaySize(row.bytes)} · {pct(row.bytes)}%
							</span>
						</div>
						<div class="bg-muted mt-1 h-1.5 overflow-hidden rounded-full">
							<div
								class="h-full rounded-full {row.warn ? 'bg-warning' : 'bg-primary/70'}"
								style="width: {pct(row.bytes)}%"
							></div>
						</div>
						{#if row.hint}
							<div class="text-muted-foreground/70 mt-0.5 text-xs">{row.hint}</div>
						{/if}
					</div>
				{/each}
			</div>

			{#if memory.workspace}
				<div class="text-muted-foreground/60 mt-4 truncate font-mono text-xs">
					ws: {memory.workspace}
				</div>
			{/if}
		{/if}
	</div>
</div>
