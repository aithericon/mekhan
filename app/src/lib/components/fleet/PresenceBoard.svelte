<script lang="ts">
	// Presence pool board — the Board tab of a runner group's detail (/fleet/[id]).
	// ALWAYS scoped to one group (the `group` prop), so it deliberately does NOT
	// repeat the group name / online count — the page header already shows those.
	// It renders just two things:
	//   1. a compact PER-GROUP coverage strip (CoverageStrip): one chip per backend
	//      the group's present runners advertise, with how many cover it, + a
	//      "View pool net" deep-link and the poll freshness;
	//   2. the station grid — one FleetCard per runner with its live backends.
	import Server from '@lucide/svelte/icons/server';
	import ArrowUpRight from '@lucide/svelte/icons/arrow-up-right';
	import {
		listRunners,
		getRunnerPresence,
		type RunnerSummary,
		type RunnerPresenceSnapshot
	} from '$lib/api/runners';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import { capacityTarget } from '$lib/editor/deployment-run-target';
	import { filterFleetByGroup } from './grouping';
	import { fmtMsAgo, fmtDate } from './format';
	import CoverageStrip from './CoverageStrip.svelte';
	import FleetCard from './FleetCard.svelte';
	import FleetEmpty from './FleetEmpty.svelte';

	type Props = {
		/** The group alias (the presence `capacity` path) this board is scoped to. */
		group?: string | null;
	};
	let { group = null }: Props = $props();

	// ── State ──────────────────────────────────────────────────────────────────

	let runners = $state<RunnerSummary[]>([]);
	let presence = $state<RunnerPresenceSnapshot[]>([]);
	let groups = $state<ResourceSummary[]>([]);
	let error = $state<string | null>(null);
	let lastUpdated = $state<Date | null>(null);

	// ── Derived ────────────────────────────────────────────────────────────────

	const presenceById = $derived(Object.fromEntries(presence.map((p) => [p.runner_id, p])));

	/** The backing group resource (for the pool-net deep-link). */
	const groupResource = $derived<ResourceSummary | null>(groups[0] ?? null);

	/** Per-backend coverage for THIS group: how many present runners advertise each
	 *  backend. Replaces the old number-less `covers` chips. */
	const coverage = $derived.by<{ backend: string; count: number }[]>(() => {
		const counts = new Map<string, number>();
		for (const r of runners) {
			const snap = presenceById[r.id];
			if (!snap?.present) continue;
			for (const be of snap.backends ?? []) counts.set(be, (counts.get(be) ?? 0) + 1);
		}
		return [...counts.entries()]
			.map(([backend, count]) => ({ backend, count }))
			.sort((a, b) => a.backend.localeCompare(b.backend));
	});

	// ── Polling ────────────────────────────────────────────────────────────────

	async function poll() {
		try {
			const [rPage, pSnaps, gPage] = await Promise.all([
				listRunners({ perPage: 200 }),
				getRunnerPresence(),
				listResources({ resource_type: 'capacity', perPage: 200 })
			]);
			presence = pSnaps;
			// A runner group is a presence `capacity` (the instrument preset). Scope
			// runners + the backing resource to this group alias.
			const allGroups = gPage.items.filter((r) => capacityTarget(r) === 'runner_group');
			const filtered = filterFleetByGroup(rPage.items, allGroups, group);
			runners = filtered.runners;
			groups = filtered.groupResources;
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
</script>

<div class="space-y-4" data-testid="presence-board">
	{#if error}
		<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
			{error}
		</div>
	{/if}

	{#if runners.length === 0 && groups.length === 0}
		<FleetEmpty message="No runners enrolled.">
			{#snippet icon()}<Server class="size-10 text-muted-foreground/40" />{/snippet}
		</FleetEmpty>
	{:else}
		<!-- Coverage strip + pool-net link + freshness. No group-name/online heading
			 here — the page header already carries those. -->
		<div class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
			<CoverageStrip entries={coverage} />
			<div class="flex items-center gap-3 text-sm text-muted-foreground">
				{#if groupResource}
					<a
						href="/nets/pool-{groupResource.id}"
						class="inline-flex items-center gap-1 hover:text-foreground hover:underline"
						data-testid="view-pool-net"
					>
						View pool net
						<ArrowUpRight class="size-3.5" />
					</a>
				{/if}
				{#if lastUpdated}
					<span class="tabular-nums">updated {lastUpdated.toLocaleTimeString()}</span>
				{/if}
			</div>
		</div>

		{#if runners.length === 0}
			<p class="px-1 py-2 text-sm text-muted-foreground">No runners enrolled in this group.</p>
		{:else}
			<div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
				{#each runners as runner (runner.id)}
					{@const snap = presenceById[runner.id]}
					{@const online = snap?.present ?? false}
					<FleetCard
						title={runner.name}
						tone={online ? 'live' : 'idle'}
						meta={online && snap
							? `Online · ${fmtMsAgo(snap.last_seen_ms_ago)}`
							: `Offline · ${fmtDate(runner.last_seen_at)}`}
						backends={online ? (snap?.backends ?? []) : []}
						testid="station-{runner.id}"
					>
						{#snippet tooltip()}
							<p class="font-mono text-sm">{runner.id}</p>
							<p class="text-sm">Status: {runner.status}</p>
							<p class="text-sm">Enrolled: {fmtDate(runner.enrolled_at)}</p>
							{#if snap && (snap.backends?.length ?? 0) > 0}
								<p class="max-w-xs text-sm">Backends: {(snap.backends ?? []).join(', ')}</p>
							{/if}
						{/snippet}
					</FleetCard>
				{/each}
			</div>
		{/if}
	{/if}
</div>
