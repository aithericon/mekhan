<script lang="ts">
	// Fleet → Worker pool board.
	// Polls getWorkerCoverage() every 5 s. Renders two sections:
	//   1. Backend coverage grid — EVERY ExecutorJob backend with its live
	//      worker count; uncovered (count 0) backends are flagged (steps on them
	//      queue at `submitted` until a worker connects).
	//   2. Connected workers — each anonymous competing-consumer worker (a FleetCard,
	//      shared with the Live board's runner stations) with the backends it serves
	//      + heartbeat freshness.
	// Distinct from the Runners/Live-board tabs: workers are NOT enrolled runners
	// (no identity, no scoped creds) — they're capacity, not held units.
	import { Badge } from '$lib/components/ui/badge';
	import Cpu from '@lucide/svelte/icons/cpu';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import {
		getWorkerCoverage,
		type WorkerCoverageEntry,
		type BackendCoverageEntry
	} from '$lib/api/workers';
	import { fmtMsAgo } from './format';
	import BoardHeader from './BoardHeader.svelte';
	import StatusDot from './StatusDot.svelte';
	import FleetCard from './FleetCard.svelte';
	import FleetEmpty from './FleetEmpty.svelte';

	// ── State ──────────────────────────────────────────────────────────────────

	let workers = $state<WorkerCoverageEntry[]>([]);
	let backends = $state<BackendCoverageEntry[]>([]);
	let error = $state<string | null>(null);
	let lastUpdated = $state<Date | null>(null);

	// ── Derived ────────────────────────────────────────────────────────────────

	const coveredCount = $derived(backends.filter((b) => b.worker_count > 0).length);
	const uncovered = $derived(backends.filter((b) => b.worker_count === 0));

	// ── Polling ────────────────────────────────────────────────────────────────

	async function poll() {
		try {
			const snap = await getWorkerCoverage();
			workers = snap.workers;
			backends = snap.backends;
			lastUpdated = new Date();
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to fetch worker coverage';
		}
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => {
			void poll();
		}, 5000);
		return () => clearInterval(t);
	});
</script>

<div class="space-y-6" data-testid="worker-pool-board">
	<BoardHeader
		title="Workers"
		summary="{workers.length} worker{workers.length === 1 ? '' : 's'} · {coveredCount} / {backends.length} backends covered"
		updated={lastUpdated}
	/>

	{#if error}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	<!-- Uncovered-backend warning banner -->
	{#if uncovered.length > 0}
		<div
			class="flex items-start gap-2 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800"
			data-testid="uncovered-warning"
		>
			<TriangleAlert class="mt-0.5 size-4 shrink-0" />
			<div>
				<span class="font-medium"
					>{uncovered.length} backend{uncovered.length === 1 ? '' : 's'} uncovered.</span
				>
				Steps using {uncovered.map((b) => b.display_name).join(', ')} will queue at
				<code class="font-mono">submitted</code> until a worker serving them connects.
			</div>
		</div>
	{/if}

	<!-- Backend coverage grid -->
	<div class="space-y-2">
		<h4 class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
			Backend coverage
		</h4>
		<div class="grid gap-2 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
			{#each backends as b (b.backend)}
				{@const covered = b.worker_count > 0}
				<div
					class="flex items-center justify-between gap-2 rounded-lg border bg-card px-3 py-2.5 transition-colors
						{covered ? 'border-emerald-200 dark:border-emerald-800/50' : 'border-amber-200 dark:border-amber-800/50'}"
					data-testid="backend-{b.backend}"
				>
					<div class="flex min-w-0 items-center gap-2">
						<StatusDot tone={covered ? 'live' : 'warn'} />
						<span class="truncate text-sm font-medium text-foreground">{b.display_name}</span>
					</div>
					<Badge variant={covered ? 'secondary' : 'outline'} class="shrink-0 text-sm tabular-nums">
						{b.worker_count}
					</Badge>
				</div>
			{/each}
		</div>
	</div>

	<!-- Connected workers -->
	<div class="space-y-2">
		<h4 class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
			Connected workers
		</h4>
		{#if workers.length === 0}
			<FleetEmpty
				message="No workers connected."
				hint="Start an executor daemon — it advertises its backends on connect."
			>
				{#snippet icon()}<Cpu class="size-10 text-muted-foreground/40" />{/snippet}
			</FleetEmpty>
		{:else}
			<div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
				{#each workers as w (w.worker_id)}
					<FleetCard
						title={w.worker_id}
						tone="live"
						meta="{w.backends.length} backend{w.backends.length === 1 ? '' : 's'} · {fmtMsAgo(
							w.last_seen_ms_ago
						)}"
						backends={w.backends}
						testid="worker-{w.worker_id}"
					>
						{#snippet tooltip()}
							<p class="font-mono text-sm">{w.worker_id}</p>
							<p class="text-sm">Serves: {w.backends.join(', ')}</p>
						{/snippet}
					</FleetCard>
				{/each}
			</div>
		{/if}
	</div>
</div>
