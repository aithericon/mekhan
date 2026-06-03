<script lang="ts">
	// Fleet → Live presence board (Section F).
	// Polls getRunnerPresence() + listRunners() every 5 s.
	// Renders a station GRID: online dot, name, pool, capability keys.
	// "Held" state is best-effort — presence snapshot carries `present` only;
	// full net-state link would require resolving the net_id from the engine
	// at /petri/nets/{id}/state — deferred (see TODO below).
	import { Badge } from '$lib/components/ui/badge';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import Server from '@lucide/svelte/icons/server';
	import { listRunners, getRunnerPresence, type RunnerSummary, type RunnerPresenceSnapshot } from '$lib/api/runners';

	// ── State ──────────────────────────────────────────────────────────────────

	let runners = $state<RunnerSummary[]>([]);
	let presence = $state<RunnerPresenceSnapshot[]>([]);
	let error = $state<string | null>(null);
	let lastUpdated = $state<Date | null>(null);

	// ── Derived ────────────────────────────────────────────────────────────────

	const presenceById = $derived(
		Object.fromEntries(presence.map((p) => [p.runner_id, p]))
	);

	/** All runners, enriched with their live presence snapshot (if any). */
	const stations = $derived(
		runners.map((r) => ({
			runner: r,
			snap: presenceById[r.id] ?? null
		}))
	);

	const onlineCount = $derived(stations.filter((s) => s.snap?.present).length);

	// ── Polling ────────────────────────────────────────────────────────────────

	async function poll() {
		try {
			const [rPage, pSnaps] = await Promise.all([
				listRunners({ perPage: 200 }),
				getRunnerPresence()
			]);
			runners = rPage.items;
			presence = pSnaps;
			lastUpdated = new Date();
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to fetch presence';
		}
	}

	$effect(() => {
		void poll();
		const t = setInterval(() => { void poll(); }, 5000);
		return () => clearInterval(t);
	});

	// ── Helpers ────────────────────────────────────────────────────────────────

	function fmtMsAgo(ms: number): string {
		if (ms < 2000) return 'just now';
		if (ms < 60_000) return `${Math.round(ms / 1000)}s ago`;
		if (ms < 3_600_000) return `${Math.round(ms / 60_000)}m ago`;
		return `${Math.round(ms / 3_600_000)}h ago`;
	}

	function fmtDate(ts: string | null | undefined): string {
		if (!ts) return '—';
		const d = new Date(ts);
		return Number.isNaN(d.getTime()) ? ts : d.toLocaleString();
	}

	// TODO: for "held" state, resolve the pool net_id from the runner's pool
	// assignment and read its marking via GET /petri/nets/{id}/state (through
	// the /api/v1/* proxy). A token in t_pool_held would mean a job is in flight
	// on this runner. The instances UI does this via the engine net-state path;
	// a future iteration can link held stations directly to their instance detail.
</script>

<div class="space-y-4" data-testid="presence-board">
	<!-- Header bar -->
	<div class="flex items-center justify-between">
		<div class="flex items-center gap-3">
			<h3 class="text-sm font-medium text-foreground">Live board</h3>
			<span class="text-sm text-muted-foreground">
				{onlineCount} / {stations.length} online
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

	{#if stations.length === 0}
		<div
			class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16"
		>
			<Server class="size-10 text-muted-foreground/40" />
			<p class="mt-3 text-sm text-muted-foreground">No runners enrolled.</p>
		</div>
	{:else}
		<!-- Station grid -->
		<div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
			{#each stations as { runner, snap } (runner.id)}
				{@const online = snap?.present ?? false}
				<Tooltip.Provider>
					<Tooltip.Root>
						<Tooltip.Trigger class="w-full text-left">
							<div
								class="flex flex-col gap-2 rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/40
									{online ? 'border-emerald-200 dark:border-emerald-800/50' : ''}"
								data-testid="station-{runner.id}"
							>
								<!-- Status row -->
								<div class="flex items-center justify-between gap-2">
									<div class="flex items-center gap-2 min-w-0">
										<span
											class="inline-block size-2.5 shrink-0 rounded-full {online
												? 'bg-emerald-500'
												: 'bg-muted-foreground/30'}"
										></span>
										<span class="truncate text-sm font-medium text-foreground">
											{runner.name}
										</span>
									</div>
									{#if runner.pool}
										<Badge variant="secondary" class="shrink-0 text-xs">
											{runner.pool}
										</Badge>
									{/if}
								</div>

								<!-- Status text -->
								<p class="text-xs text-muted-foreground">
									{#if online && snap}
										Online · {fmtMsAgo(snap.last_seen_ms_ago)}
									{:else}
										Offline · {fmtDate(runner.last_seen_at)}
									{/if}
								</p>
							</div>
						</Tooltip.Trigger>
						<Tooltip.Content side="top">
							<p class="font-mono text-xs">{runner.id}</p>
							<p class="text-xs">Status: {runner.status}</p>
							<p class="text-xs">Enrolled: {fmtDate(runner.enrolled_at)}</p>
						</Tooltip.Content>
					</Tooltip.Root>
				</Tooltip.Provider>
			{/each}
		</div>
	{/if}
</div>
