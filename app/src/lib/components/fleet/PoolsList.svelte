<script lang="ts">
	// The Pools tab body: every capacity as ONE ROW, grouped by operator-vocabulary
	// kind from the pinned taxonomy in `pool-kinds.ts` (Machine pools / Worker
	// pools / Human pools / Limits / Clusters / Not dispatchable). Each section
	// renders even when empty — with its one-line empty message — EXCEPT `broken`,
	// which only appears when such rows exist (legacy pre-acceptance rows,
	// fail-closed; kept visible so an admin can recreate or delete them).
	import PoolRow from './PoolRow.svelte';
	import { POOL_KINDS, POOL_KIND_ORDER, poolKindOf } from './pool-kinds';
	import type { CapacitySummary } from '$lib/api/capacities';

	let {
		capacities,
		onenroll,
		onenrollworker,
		onedit,
		ondelete,
		onreconnect,
		ondrain
	}: {
		capacities: CapacitySummary[];
		/** Machine pool "Enroll" — open the runner EnrollSheet scoped to that path. */
		onenroll?: (path: string, isPlatform: boolean) => void;
		/** Worker pool "Enroll" — open the worker-mode EnrollSheet scoped to that path. */
		onenrollworker?: (path: string, isPlatform: boolean) => void;
		/** Row overflow → Edit (NewCapacityModal in edit mode). */
		onedit?: (id: string) => void;
		/** Row overflow → Delete (confirm + deleteResource). */
		ondelete?: (id: string) => void;
		/** Cluster row "Reconnect". */
		onreconnect?: (id: string) => void;
		/** Cluster row "Drain". */
		ondrain?: (id: string) => void;
	} = $props();

	// Partition once per poll into the pinned section order; `broken` drops out
	// when empty.
	const sections = $derived(
		POOL_KIND_ORDER.map((kindId) => ({
			kind: POOL_KINDS[kindId],
			pools: capacities.filter((c) => poolKindOf(c).id === kindId)
		})).filter((s) => s.kind.id !== 'broken' || s.pools.length > 0)
	);
</script>

<div class="space-y-8">
	{#each sections as section (section.kind.id)}
		<section>
			<h2
				class="mb-2 flex items-baseline gap-1.5 border-b border-border pb-1.5 text-xs font-semibold tracking-wider text-muted-foreground uppercase"
			>
				{section.kind.label}
				<span class="font-normal tracking-normal normal-case">· {section.pools.length}</span>
			</h2>
			{#if section.pools.length === 0}
				<p class="text-sm text-muted-foreground">{section.kind.empty}</p>
			{:else}
				<div class="divide-y divide-border overflow-hidden rounded-lg border border-border bg-card">
					{#each section.pools as capacity (capacity.id)}
						<PoolRow {capacity} {onenroll} {onenrollworker} {onedit} {ondelete} {onreconnect} {ondrain} />
					{/each}
				</div>
			{/if}
		</section>
	{/each}
</div>
