<script lang="ts">
	// One Control-Plane capacity card. Renders a backend-appropriate live line
	// from `capacity.live.kind`:
	//   tokens    → "N seeded · M in use"
	//   presence  → StatusDot + "X/Y online" + advertised backends
	//   queue     → StatusDot + "X/Y online" + advertised backends
	//   scheduler → labelled stat grid (flavor / watcher / leases / success%)
	// Card actions are placeholders this phase (Phase 3 wires them): presence
	// cards link to /fleet/{id} + offer "Enroll here"; scheduler cards link to
	// /clusters/{id} + offer reconnect/drain; all cards expose an edit/delete
	// affordance. Semantic Tailwind tokens only so the dashboard theme applies.
	import { Button } from '$lib/components/ui/button';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import StatusDot from './StatusDot.svelte';
	import BackendChips from './BackendChips.svelte';
	import type { CapacitySummary } from '$lib/api/capacities';

	let {
		capacity,
		onedit,
		ondelete,
		onenroll,
		onreconnect,
		ondrain
	}: {
		capacity: CapacitySummary;
		/** Edit affordance — opens the create modal (no id-prefill yet). */
		onedit?: (id: string) => void;
		/** Delete the capacity resource (retires its backing net). */
		ondelete?: (id: string) => void;
		/** Presence-card "Enroll here" — the page opens an EnrollSheet scoped to
		 *  this group (passes the capacity `path`). */
		onenroll?: (path: string) => void;
		/** Scheduler-card reconnect — force the cluster session to reconnect. */
		onreconnect?: (id: string) => void;
		/** Scheduler-card drain — refuse new leases while held ones finish. */
		ondrain?: (id: string) => void;
	} = $props();

	const live = $derived(capacity.live);

	// Liveness tone for the leading dot — presence/queue/scheduler carry one.
	const tone = $derived.by<'live' | 'idle' | 'warn'>(() => {
		if (live.kind === 'presence') return live.online > 0 ? 'live' : 'idle';
		if (live.kind === 'queue') return live.online > 0 ? 'live' : 'idle';
		if (live.kind === 'scheduler') {
			if (live.draining) return 'warn';
			return live.watcher_state === 'connected' ? 'live' : 'idle';
		}
		return 'idle';
	});

	const showDot = $derived(
		live.kind === 'presence' || live.kind === 'queue' || live.kind === 'scheduler'
	);

	function pct(rate: number | null | undefined): string {
		if (rate == null) return '—';
		return `${Math.round(rate * 100)}%`;
	}

	// Per-backend deep-link target ('→ detail'). Presence → the runner fleet
	// board scoped to this group; scheduler → the cluster's lease view.
	const detailHref = $derived.by<string | null>(() => {
		if (capacity.backend === 'presence') return `/fleet/${capacity.id}`;
		if (capacity.backend === 'queue') return `/fleet/${capacity.id}`;
		if (capacity.backend === 'scheduler') return `/clusters/${capacity.id}`;
		return null;
	});
</script>

<div
	class="flex flex-col gap-3 rounded-lg border bg-card p-4 transition-colors hover:bg-accent/40
		{tone === 'live' ? 'border-emerald-200 dark:border-emerald-800/50' : 'border-border'}"
	data-testid="capacity-card-{capacity.id}"
>
	<!-- Header: name + path -->
	<div class="flex min-w-0 items-start gap-2">
		{#if showDot}<StatusDot {tone} class="mt-1" />{/if}
		<div class="min-w-0 flex-1">
			<p class="truncate text-sm font-medium text-foreground">{capacity.display_name}</p>
			<p class="truncate font-mono text-sm text-muted-foreground">{capacity.path}</p>
		</div>
	</div>

	<!-- Live line — backend-discriminated -->
	<div class="text-sm text-muted-foreground" data-testid="capacity-live">
		{#if live.kind === 'tokens'}
			<span class="text-foreground tabular-nums">{live.seeded}</span> seeded ·
			<span class="text-foreground tabular-nums">{live.in_use}</span> in use
		{:else if live.kind === 'presence'}
			<div class="flex flex-col gap-2">
				<span>
					<span class="text-foreground tabular-nums">{live.online}</span>/<span class="tabular-nums"
						>{live.total}</span
					> online
				</span>
				{#if live.backends.length > 0}
					<BackendChips backends={live.backends} />
				{/if}
			</div>
		{:else if live.kind === 'queue'}
			<div class="flex flex-col gap-2">
				<span>
					<span class="text-foreground tabular-nums">{live.online}</span>/<span class="tabular-nums"
						>{live.enrolled}</span
					> online
				</span>
				{#if live.backends.length > 0}
					<BackendChips backends={live.backends} />
				{/if}
			</div>
		{:else if live.kind === 'scheduler'}
			<div class="grid grid-cols-2 gap-x-4 gap-y-2 text-sm" data-testid="capacity-scheduler-stats">
				<div class="flex flex-col">
					<span class="text-xs text-muted-foreground">Flavor</span>
					<span class="truncate text-foreground tabular-nums">{live.flavor}</span>
				</div>
				<div class="flex flex-col">
					<span class="text-xs text-muted-foreground">Watcher</span>
					<span class="flex items-center gap-1.5 text-foreground">
						<StatusDot {tone} />
						<span class="truncate tabular-nums">{live.watcher_state}</span>
					</span>
				</div>
				<div class="flex flex-col">
					<span class="text-xs text-muted-foreground">Active leases</span>
					<span class="text-foreground tabular-nums">{live.active_leases}</span>
				</div>
				<div class="flex flex-col">
					<span class="text-xs text-muted-foreground">Success</span>
					<span class="text-foreground tabular-nums">{pct(live.success_rate)}</span>
				</div>
				{#if live.draining}
					<div class="col-span-2">
						<span
							class="inline-flex items-center rounded-md border border-amber-200 bg-amber-50 px-2 py-0.5 text-xs font-medium text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/40 dark:text-amber-200"
						>
							Draining
						</span>
					</div>
				{/if}
			</div>
		{:else}
			<span>No live data</span>
		{/if}
	</div>

	<!-- Actions -->
	<div class="flex items-center gap-1 border-t border-border pt-2">
		{#if capacity.backend === 'presence' || capacity.backend === 'queue'}
			<Button
				variant="ghost"
				size="sm"
				class="text-sm text-muted-foreground"
				onclick={() => onenroll?.(capacity.path)}
				data-testid="capacity-enroll-{capacity.id}"
			>
				Enroll here
			</Button>
		{:else if capacity.backend === 'scheduler'}
			<Button
				variant="ghost"
				size="sm"
				class="text-sm text-muted-foreground"
				onclick={() => onreconnect?.(capacity.id)}
				data-testid="capacity-reconnect-{capacity.id}"
			>
				Reconnect
			</Button>
			<Button
				variant="ghost"
				size="sm"
				class="text-sm text-muted-foreground"
				onclick={() => ondrain?.(capacity.id)}
				data-testid="capacity-drain-{capacity.id}"
			>
				{live.kind === 'scheduler' && live.draining ? 'Draining' : 'Drain'}
			</Button>
		{/if}

		{#if detailHref}
			<Button
				variant="ghost"
				size="sm"
				class="gap-1 text-sm text-muted-foreground"
				href={detailHref}
				data-testid="capacity-detail-{capacity.id}"
			>
				Detail
				<ArrowRight class="size-3.5" />
			</Button>
		{/if}

		<div class="ml-auto flex items-center gap-1">
			<Button
				variant="ghost"
				size="sm"
				class="text-muted-foreground"
				onclick={() => onedit?.(capacity.id)}
				title="Edit"
			>
				<Pencil class="size-3.5" />
			</Button>
			<Button
				variant="ghost"
				size="sm"
				class="text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
				onclick={() => ondelete?.(capacity.id)}
				title="Delete"
			>
				<Trash2 class="size-3.5" />
			</Button>
		</div>
	</div>
</div>
