<script lang="ts">
	// One backend section of the Control Plane — its top-level sections ARE the
	// four backends (Presence / Queue / Tokens / Scheduler), mirroring
	// `CapacityAxes::backend()` 1:1. Renders a backend header (label + an
	// aggregate count derived from the section's live data) + a responsive card
	// grid + an optional section action slot. Empty → FleetEmpty.
	import type { Snippet } from 'svelte';
	import type { CapacitySummary } from '$lib/api/capacities';
	import CapacityCard from './CapacityCard.svelte';
	import FleetEmpty from './FleetEmpty.svelte';

	let {
		title,
		backend,
		capacities,
		emptyMessage,
		action,
		emptyIcon,
		onedit,
		ondelete,
		onenroll,
		onreconnect,
		ondrain
	}: {
		title: string;
		/** The backend this section groups (used for the testid + noun). */
		backend: CapacitySummary['backend'];
		capacities: CapacitySummary[];
		/** Empty-state copy. */
		emptyMessage?: string;
		/** Optional right-aligned section action (e.g. "Enroll runner"). */
		action?: Snippet;
		/** Optional empty-state glyph. */
		emptyIcon?: Snippet;
		onedit?: (id: string) => void;
		ondelete?: (id: string) => void;
		/** Forwarded to presence cards' "Enroll here" (passes the group `path`). */
		onenroll?: (path: string) => void;
		/** Forwarded to scheduler cards' reconnect/drain actions. */
		onreconnect?: (id: string) => void;
		ondrain?: (id: string) => void;
	} = $props();

	// The aggregate count line under each backend header — backend-shaped, the
	// same vocabulary the cards use. e.g. "3 groups · 7/9 online".
	const summary = $derived.by<string>(() => {
		const n = capacities.length;
		if (n === 0) return 'none';
		if (backend === 'presence') {
			const noun = n === 1 ? 'group' : 'groups';
			let online = 0;
			let total = 0;
			for (const c of capacities) {
				if (c.live.kind === 'presence') {
					online += c.live.online;
					total += c.live.total;
				}
			}
			return `${n} ${noun} · ${online}/${total} online`;
		}
		if (backend === 'queue') {
			const noun = n === 1 ? 'group' : 'groups';
			let online = 0;
			let enrolled = 0;
			for (const c of capacities) {
				if (c.live.kind === 'queue') {
					online += c.live.online;
					enrolled += c.live.enrolled;
				}
			}
			return `${n} ${noun} · ${online}/${enrolled} online`;
		}
		if (backend === 'tokens') {
			const noun = n === 1 ? 'limit' : 'limits';
			let seeded = 0;
			let inUse = 0;
			for (const c of capacities) {
				if (c.live.kind === 'tokens') {
					seeded += c.live.seeded;
					inUse += c.live.in_use;
				}
			}
			return `${n} ${noun} · ${inUse}/${seeded} in use`;
		}
		if (backend === 'scheduler') {
			const noun = n === 1 ? 'cluster' : 'clusters';
			let leases = 0;
			for (const c of capacities) if (c.live.kind === 'scheduler') leases += c.live.active_leases;
			return `${n} ${noun} · ${leases} active leases`;
		}
		const noun = n === 1 ? 'capacity' : 'capacities';
		return `${n} ${noun}`;
	});
</script>

<section data-testid="capacity-section-{backend}" class="space-y-3">
	<div class="flex items-center justify-between">
		<div class="flex items-baseline gap-3">
			<h2 class="text-sm font-semibold tracking-tight text-foreground">{title}</h2>
			<span class="text-sm text-muted-foreground tabular-nums">{summary}</span>
		</div>
		{#if action}
			<div>{@render action()}</div>
		{/if}
	</div>

	{#if capacities.length === 0}
		<FleetEmpty message={emptyMessage ?? `No ${title.toLowerCase()} capacities.`}>
			{#snippet icon()}{#if emptyIcon}{@render emptyIcon()}{/if}{/snippet}
		</FleetEmpty>
	{:else}
		<div
			class="grid gap-3 {backend === 'scheduler'
				? 'sm:grid-cols-1 lg:grid-cols-2'
				: 'sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4'}"
		>
			{#each capacities as capacity (capacity.id)}
				<CapacityCard {capacity} {onedit} {ondelete} {onenroll} {onreconnect} {ondrain} />
			{/each}
		</div>
	{/if}
</section>
