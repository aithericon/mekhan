<script lang="ts">
	// Fleet → Worker pool board.
	// Polls getWorkerCoverage() every 5 s. Renders two sections:
	//   1. Backend coverage grid — EVERY ExecutorJob backend with its live
	//      worker count; uncovered (count 0) backends are flagged (steps on them
	//      queue at `submitted` until a worker connects).
	//   2. Connected workers — each anonymous competing-consumer worker with the
	//      backends it serves + heartbeat freshness.
	// Distinct from the Runners/Live-board tabs: workers are NOT enrolled runners
	// (no identity, no scoped creds) — they're capacity, not held units.
	import { Badge } from '$lib/components/ui/badge';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import Cpu from '@lucide/svelte/icons/cpu';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import {
		getWorkerCoverage,
		type WorkerCoverageEntry,
		type BackendCoverageEntry
	} from '$lib/api/workers';

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

	// ── Helpers ────────────────────────────────────────────────────────────────

	function fmtMsAgo(ms: number): string {
		if (ms < 2000) return 'just now';
		if (ms < 60_000) return `${Math.round(ms / 1000)}s ago`;
		if (ms < 3_600_000) return `${Math.round(ms / 60_000)}m ago`;
		return `${Math.round(ms / 3_600_000)}h ago`;
	}
</script>

<div class="space-y-6" data-testid="worker-pool-board">
	<!-- Header bar -->
	<div class="flex items-center justify-between">
		<div class="flex items-center gap-3">
			<h3 class="text-sm font-medium text-foreground">Workers</h3>
			<span class="text-sm text-muted-foreground">
				{workers.length} worker{workers.length === 1 ? '' : 's'} · {coveredCount} / {backends.length}
				backends covered
			</span>
		</div>
		{#if lastUpdated}
			<span class="text-xs text-muted-foreground tabular-nums">
				Updated {lastUpdated.toLocaleTimeString()}
			</span>
		{/if}
	</div>

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
				<span class="font-medium">{uncovered.length} backend{uncovered.length === 1 ? '' : 's'} uncovered.</span>
				Steps using {uncovered.map((b) => b.display_name).join(', ')} will queue at
				<code class="font-mono">submitted</code> until a worker serving them connects.
			</div>
		</div>
	{/if}

	<!-- Backend coverage grid -->
	<div class="space-y-2">
		<h4 class="text-xs font-medium uppercase tracking-wide text-muted-foreground">Backend coverage</h4>
		<div class="grid gap-2 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
			{#each backends as b (b.backend)}
				{@const covered = b.worker_count > 0}
				<div
					class="flex items-center justify-between gap-2 rounded-lg border bg-card px-3 py-2.5 transition-colors
						{covered ? 'border-emerald-200 dark:border-emerald-800/50' : 'border-amber-200 dark:border-amber-800/50'}"
					data-testid="backend-{b.backend}"
				>
					<div class="flex items-center gap-2 min-w-0">
						<span
							class="inline-block size-2.5 shrink-0 rounded-full {covered
								? 'bg-emerald-500'
								: 'bg-amber-500'}"
						></span>
						<span class="truncate text-sm font-medium text-foreground">{b.display_name}</span>
					</div>
					<Badge variant={covered ? 'secondary' : 'outline'} class="shrink-0 text-xs tabular-nums">
						{b.worker_count}
					</Badge>
				</div>
			{/each}
		</div>
	</div>

	<!-- Connected workers -->
	<div class="space-y-2">
		<h4 class="text-xs font-medium uppercase tracking-wide text-muted-foreground">
			Connected workers
		</h4>
		{#if workers.length === 0}
			<div
				class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-12"
			>
				<Cpu class="size-10 text-muted-foreground/40" />
				<p class="mt-3 text-sm text-muted-foreground">No workers connected.</p>
				<p class="mt-1 text-xs text-muted-foreground">
					Start an executor daemon — it advertises its backends on connect.
				</p>
			</div>
		{:else}
			<div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
				{#each workers as w (w.worker_id)}
					<Tooltip.Provider>
						<Tooltip.Root>
							<Tooltip.Trigger class="w-full text-left">
								<div
									class="flex flex-col gap-2 rounded-lg border border-emerald-200 bg-card p-4 transition-colors hover:bg-accent/40 dark:border-emerald-800/50"
									data-testid="worker-{w.worker_id}"
								>
									<div class="flex items-center gap-2 min-w-0">
										<span class="inline-block size-2.5 shrink-0 rounded-full bg-emerald-500"></span>
										<span class="truncate text-sm font-medium text-foreground">{w.worker_id}</span>
									</div>
									<div class="flex flex-wrap gap-1">
										{#each w.backends as be (be)}
											<Badge variant="secondary" class="text-xs">{be}</Badge>
										{/each}
									</div>
									<p class="text-xs text-muted-foreground">
										{w.backends.length} backend{w.backends.length === 1 ? '' : 's'} · {fmtMsAgo(
											w.last_seen_ms_ago
										)}
									</p>
								</div>
							</Tooltip.Trigger>
							<Tooltip.Content side="top">
								<p class="font-mono text-xs">{w.worker_id}</p>
								<p class="text-xs">Serves: {w.backends.join(', ')}</p>
							</Tooltip.Content>
						</Tooltip.Root>
					</Tooltip.Provider>
				{/each}
			</div>
		{/if}
	</div>
</div>
