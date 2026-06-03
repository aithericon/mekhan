<script lang="ts">
	// Fleet → Live presence board, SPLIT INTO ITS GROUPS.
	// Polls getRunnerPresence() + listRunners() + the runner_group resources every
	// 5 s and renders one station grid per group (the capacity layer made visible):
	// each group header shows online/total + the union of backends its present
	// runners cover; an "unbacked" alias (a runner group string with no backing
	// resource → no pool net) is surfaced loudly; ungrouped runners come last.
	// Each station card shows the executor backends that runner advertises (the
	// set-membership dimension it heartbeats — what it can actually run, orthogonal
	// to its typed capabilities).
	// "Held" state is best-effort — the presence snapshot carries `present` +
	// `last_seen_ms_ago` + `backends`, but not in-flight job state; a full
	// net-state link would require resolving the net_id from the engine at
	// /petri/nets/{id}/state — deferred (see TODO below).
	import { Badge } from '$lib/components/ui/badge';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import Server from '@lucide/svelte/icons/server';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';
	import RadioTower from '@lucide/svelte/icons/radio-tower';
	import {
		listRunners,
		getRunnerPresence,
		type RunnerSummary,
		type RunnerPresenceSnapshot
	} from '$lib/api/runners';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import { groupFleet } from './grouping';

	// ── State ──────────────────────────────────────────────────────────────────

	let runners = $state<RunnerSummary[]>([]);
	let presence = $state<RunnerPresenceSnapshot[]>([]);
	let groups = $state<ResourceSummary[]>([]);
	let error = $state<string | null>(null);
	let lastUpdated = $state<Date | null>(null);

	// ── Derived ────────────────────────────────────────────────────────────────

	const presenceById = $derived(Object.fromEntries(presence.map((p) => [p.runner_id, p])));

	/** The fleet split into ordered group sections. */
	const sections = $derived(groupFleet(runners, presenceById, groups));

	const onlineCount = $derived(
		runners.filter((r) => presenceById[r.id]?.present).length
	);

	// ── Polling ────────────────────────────────────────────────────────────────

	async function poll() {
		try {
			const [rPage, pSnaps, gPage] = await Promise.all([
				listRunners({ perPage: 200 }),
				getRunnerPresence(),
				listResources({ resource_type: 'runner_group', perPage: 200 })
			]);
			runners = rPage.items;
			presence = pSnaps;
			groups = gPage.items;
			lastUpdated = new Date();
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to fetch presence';
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
				{onlineCount} / {runners.length} online
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

	{#if runners.length === 0 && groups.length === 0}
		<div
			class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16"
		>
			<Server class="size-10 text-muted-foreground/40" />
			<p class="mt-3 text-sm text-muted-foreground">No runners enrolled.</p>
		</div>
	{:else}
		<div class="space-y-6">
			{#each sections as section (section.kind + ':' + (section.alias ?? '∅'))}
				<section data-testid="board-section-{section.alias ?? 'ungrouped'}">
					<!-- Section header -->
					{#if section.kind === 'unbacked'}
						<div
							class="mb-2 flex flex-wrap items-center gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2"
						>
							<TriangleAlert class="size-4 shrink-0 text-amber-600" />
							<span class="font-medium text-amber-800 dark:text-amber-400">{section.alias}</span>
							<Badge variant="outline" class="border-amber-500/50 text-amber-700 dark:text-amber-400"
								>no pool · unbacked</Badge
							>
							<span class="text-xs text-amber-700/90 dark:text-amber-400/80">
								Online here, but admitted to no pool — create the
								<code class="font-mono">runner_group</code> resource to back it.
							</span>
						</div>
					{:else if section.kind === 'ungrouped'}
						<div class="mb-2 flex items-center gap-2 border-b border-border pb-1.5">
							<span class="text-sm font-semibold text-muted-foreground">Ungrouped</span>
							<span class="text-xs text-muted-foreground">· not assigned to a group</span>
						</div>
					{:else}
						<div class="mb-2 flex flex-wrap items-center gap-2 border-b border-border pb-1.5">
							<RadioTower class="size-4 shrink-0 text-muted-foreground" />
							<span class="text-sm font-semibold text-foreground">{section.alias}</span>
							<Badge variant="outline" class="text-xs">pool ready</Badge>
							<span class="text-xs text-muted-foreground tabular-nums">
								{section.onlineCount}/{section.runners.length} online
							</span>
							{#if section.backends.length > 0}
								<span class="ml-1 text-xs text-muted-foreground">covers</span>
								<div class="flex flex-wrap gap-1">
									{#each section.backends as be (be)}
										<Badge
											variant="outline"
											class="px-1.5 py-0 text-[10px] font-normal text-muted-foreground">{be}</Badge
										>
									{/each}
								</div>
							{/if}
						</div>
					{/if}

					{#if section.runners.length === 0}
						<p class="px-1 py-2 text-xs text-muted-foreground">No runners enrolled in this group.</p>
					{:else}
						<!-- Station grid -->
						<div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
							{#each section.runners as runner (runner.id)}
								{@const snap = presenceById[runner.id]}
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
												</div>

												<!-- Status text -->
												<p class="text-xs text-muted-foreground">
													{#if online && snap}
														Online · {fmtMsAgo(snap.last_seen_ms_ago)}
													{:else}
														Offline · {fmtDate(runner.last_seen_at)}
													{/if}
												</p>

												<!-- Advertised backends (the executor backends this runner
												     can run — its set-membership dimension). Shown only while
												     online; an offline runner isn't advertising anything. -->
												{#if online && snap && (snap.backends?.length ?? 0) > 0}
													<div
														class="flex flex-wrap gap-1 pt-0.5"
														data-testid="station-backends-{runner.id}"
													>
														{#each snap.backends ?? [] as be (be)}
															<Badge
																variant="outline"
																class="px-1.5 py-0 text-[10px] font-normal text-muted-foreground"
															>
																{be}
															</Badge>
														{/each}
													</div>
												{/if}
											</div>
										</Tooltip.Trigger>
										<Tooltip.Content side="top">
											<p class="font-mono text-xs">{runner.id}</p>
											<p class="text-xs">Status: {runner.status}</p>
											<p class="text-xs">Enrolled: {fmtDate(runner.enrolled_at)}</p>
											{#if snap && (snap.backends?.length ?? 0) > 0}
												<p class="max-w-xs text-xs">Backends: {(snap.backends ?? []).join(', ')}</p>
											{/if}
										</Tooltip.Content>
									</Tooltip.Root>
								</Tooltip.Provider>
							{/each}
						</div>
					{/if}
				</section>
			{/each}
		</div>
	{/if}
</div>
