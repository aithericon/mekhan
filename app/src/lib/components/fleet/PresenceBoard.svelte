<script lang="ts">
	// Fleet → Live presence board, split into its groups.
	// Polls getRunnerPresence() + listRunners() + the presence `capacity` resources
	// (the runner groups) every
	// 5 s and renders one station grid per group via the shared fleet components
	// (BoardHeader / GroupSectionHeader / FleetCard). Each station shows the
	// executor backends that runner advertises (its set-membership dimension —
	// what it can run, orthogonal to its typed capabilities).
	// "Held" state is best-effort — the presence snapshot carries `present` +
	// `last_seen_ms_ago` + `backends`, but not in-flight job state; a full
	// net-state link would require resolving the net_id from the engine at
	// /petri/nets/{id}/state — deferred (see TODO below).
	import Server from '@lucide/svelte/icons/server';
	import {
		listRunners,
		getRunnerPresence,
		type RunnerSummary,
		type RunnerPresenceSnapshot
	} from '$lib/api/runners';
	import { listResources, type ResourceSummary } from '$lib/api/resources';
	import { capacityTarget } from '$lib/editor/deployment-run-target';
	import { groupFleet, filterFleetByGroup } from './grouping';
	import { fmtMsAgo, fmtDate } from './format';
	import BoardHeader from './BoardHeader.svelte';
	import GroupSectionHeader from './GroupSectionHeader.svelte';
	import FleetCard from './FleetCard.svelte';
	import FleetEmpty from './FleetEmpty.svelte';

	type Props = {
		/** When set, constrain the board to this group alias (the capacity `path`):
		 *  only that group's runners + that backing resource are shown. Omitted ⇒
		 *  the full cross-group board (the default used by the fleet page). */
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

	/** The fleet split into ordered group sections. */
	const sections = $derived(groupFleet(runners, presenceById, groups));

	const onlineCount = $derived(runners.filter((r) => presenceById[r.id]?.present).length);

	// ── Polling ────────────────────────────────────────────────────────────────

	async function poll() {
		try {
			const [rPage, pSnaps, gPage] = await Promise.all([
				listRunners({ perPage: 200 }),
				getRunnerPresence(),
				listResources({ resource_type: 'capacity', perPage: 200 })
			]);
			presence = pSnaps;
			// A runner group is a presence `capacity` (the instrument preset).
			const allGroups = gPage.items.filter((r) => capacityTarget(r) === 'runner_group');
			// When scoped to one group, drop everything else (runners + backing res).
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

	// TODO: for "held" state, resolve the pool net_id from the runner's pool
	// assignment and read its marking via GET /petri/nets/{id}/state (through
	// the /api/v1/* proxy). A token in t_pool_held would mean a job is in flight
	// on this runner. The instances UI does this via the engine net-state path;
	// a future iteration can link held stations directly to their instance detail.
</script>

<div class="space-y-4" data-testid="presence-board">
	<BoardHeader
		title="Live board"
		summary="{onlineCount} / {runners.length} online"
		updated={lastUpdated}
	/>

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
		<div class="space-y-6">
			{#each sections as section (section.kind + ':' + (section.alias ?? '∅'))}
				<section data-testid="board-section-{section.alias ?? 'ungrouped'}">
					<GroupSectionHeader {section} />

					{#if section.runners.length === 0}
						<p class="px-1 py-2 text-sm text-muted-foreground">No runners enrolled in this group.</p>
					{:else}
						<div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
							{#each section.runners as runner (runner.id)}
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
				</section>
			{/each}
		</div>
	{/if}
</div>
