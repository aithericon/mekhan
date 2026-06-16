<script lang="ts">
	// One pool row on the Fleet → Pools tab. A bordered LIST ROW (rendered inside
	// PoolsList's divide-y container), not a card: identity on the left (liveness
	// dot · name → detail link · mono path · kind chip), the kind-appropriate live
	// line + collapsed coverage chips in the middle, actions on the right.
	//
	// Kind-specific actions (kind from `poolKindOf`, the pinned taxonomy):
	//   machine → "Enroll" opens the runner EnrollSheet scoped to this pool's path
	//   worker  → "Enroll" opens the worker EnrollSheet scoped to this pool's path
	//   human   → "Enroll" navigates to the pool detail (the roster lives there)
	//   cluster → "Reconnect" / "Drain"
	//   ALL     → overflow (⋮) menu with Edit + Delete
	// Scheduler success-rate / watcher detail intentionally stays OFF the row —
	// it lives on /clusters/{id}; the live line + draining warn dot is enough.
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import {
		DropdownMenu,
		DropdownMenuTrigger,
		DropdownMenuContent,
		DropdownMenuItem,
		DropdownMenuSeparator
	} from '$lib/components/ui/dropdown-menu';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import EllipsisVertical from '@lucide/svelte/icons/ellipsis-vertical';
	import Globe from '@lucide/svelte/icons/globe';
	import StatusDot from './StatusDot.svelte';
	import BackendChips from './BackendChips.svelte';
	import { poolKindOf, poolLiveLine } from './pool-kinds';
	import type { CapacitySummary } from '$lib/api/capacities';
	import { isPlatformCapacity, canMutateCapacity } from '$lib/api/resource-tier';

	let {
		capacity,
		onenroll,
		onenrollworker,
		onedit,
		ondelete,
		onreconnect,
		ondrain
	}: {
		capacity: CapacitySummary;
		/** Machine pool "Enroll" — open the runner EnrollSheet scoped to this path. */
		onenroll?: (path: string) => void;
		/** Worker pool "Enroll" — open the worker-mode EnrollSheet scoped to this path. */
		onenrollworker?: (path: string) => void;
		/** Overflow → Edit: opens NewCapacityModal in edit mode. */
		onedit?: (id: string) => void;
		/** Overflow → Delete: confirm + deleteResource (retires the backing net). */
		ondelete?: (id: string) => void;
		/** Cluster "Reconnect" — force the cluster session to reconnect. */
		onreconnect?: (id: string) => void;
		/** Cluster "Drain" — refuse new leases while held ones finish. */
		ondrain?: (id: string) => void;
	} = $props();

	const kind = $derived(poolKindOf(capacity));
	const live = $derived(capacity.live);
	const liveLine = $derived(poolLiveLine(capacity));

	// Platform-scoped pools (the shared worker `default` + `model_serving` pools)
	// get a sky-toned "Platform (shared)" badge and are read-only for anyone but a
	// platform admin — `canMutateCapacity` folds the platform/tenant tiers into the
	// `my_effective_role !== 'viewer'` signal the backend stamps per row.
	const isPlatform = $derived(isPlatformCapacity(capacity));
	const canMutate = $derived(canMutateCapacity(capacity));

	// Liveness tone: live when anything is online / in use / leased; warn when a
	// cluster is draining or the axes failed to parse (broken); idle otherwise.
	const tone = $derived.by<'live' | 'idle' | 'warn'>(() => {
		if (kind.id === 'broken') return 'warn';
		if (live.kind === 'scheduler' && live.draining) return 'warn';
		if (live.kind === 'presence' || live.kind === 'queue') return live.online > 0 ? 'live' : 'idle';
		if (live.kind === 'tokens') return live.in_use > 0 ? 'live' : 'idle';
		if (live.kind === 'scheduler') return live.active_leases > 0 ? 'live' : 'idle';
		return 'idle';
	});

	// Coverage chips, COLLAPSED: first 3 advertised backends + "+N more".
	const backends = $derived(
		live.kind === 'presence' || live.kind === 'queue' ? live.backends : []
	);
	const shownBackends = $derived(backends.slice(0, 3));
	const moreBackends = $derived(Math.max(0, backends.length - 3));

	// Whole-row name → detail. Every kind gets the uniform /fleet/{id} shell
	// except clusters, whose lease/watcher view lives at /clusters/{id}.
	const detailHref = $derived(
		kind.id === 'cluster' ? `/clusters/${capacity.id}` : `/fleet/${capacity.id}`
	);
</script>

<div class="flex items-center gap-3 px-4 py-3" data-testid="pool-row-{capacity.id}">
	<StatusDot {tone} />

	<!-- Identity: name (→ detail) · mono path · kind chip -->
	<div class="flex min-w-0 flex-1 flex-wrap items-center gap-x-2 gap-y-0.5">
		<a
			href={detailHref}
			class="truncate text-sm font-medium text-foreground hover:underline"
			data-testid="pool-detail-{capacity.id}"
		>
			{capacity.display_name}
		</a>
		<span class="truncate font-mono text-sm text-muted-foreground">{capacity.path}</span>
		<Badge variant="secondary" class="font-normal">{kind.chip}</Badge>
		{#if isPlatform}
			<Badge
				class="gap-1 bg-sky-100 font-normal text-sky-800"
				data-testid="pool-platform-badge-{capacity.id}"
			>
				<Globe class="size-3" /> Platform (shared)
			</Badge>
		{/if}
	</div>

	<!-- Live line + collapsed coverage chips (presence/queue only) -->
	<div class="hidden shrink-0 items-center gap-2 sm:flex">
		{#if liveLine}
			<span class="whitespace-nowrap text-sm text-muted-foreground tabular-nums">{liveLine}</span>
		{/if}
		{#if shownBackends.length > 0}
			<BackendChips backends={shownBackends} />
			{#if moreBackends > 0}
				<span class="whitespace-nowrap text-sm text-muted-foreground">+{moreBackends} more</span>
			{/if}
		{/if}
	</div>

	<!-- Actions -->
	<div class="flex shrink-0 items-center gap-1">
		{#if kind.id === 'machine'}
			<Button
				variant="ghost"
				size="sm"
				class="text-sm text-muted-foreground"
				onclick={() => onenroll?.(capacity.path)}
				data-testid="pool-enroll-{capacity.id}"
			>
				Enroll
			</Button>
		{:else if kind.id === 'worker'}
			<Button
				variant="ghost"
				size="sm"
				class="text-sm text-muted-foreground"
				onclick={() => onenrollworker?.(capacity.path)}
				data-testid="pool-enroll-{capacity.id}"
			>
				Enroll
			</Button>
		{:else if kind.id === 'human'}
			<!-- The roster lives on the pool detail page — enrolling is done there. -->
			<Button
				variant="ghost"
				size="sm"
				class="text-sm text-muted-foreground"
				href={detailHref}
				data-testid="pool-enroll-{capacity.id}"
			>
				Enroll
			</Button>
		{:else if kind.id === 'cluster' && canMutate}
			<Button
				variant="ghost"
				size="sm"
				class="text-sm text-muted-foreground"
				onclick={() => onreconnect?.(capacity.id)}
				data-testid="pool-reconnect-{capacity.id}"
			>
				Reconnect
			</Button>
			<Button
				variant="ghost"
				size="sm"
				class="text-sm text-muted-foreground"
				onclick={() => ondrain?.(capacity.id)}
				data-testid="pool-drain-{capacity.id}"
			>
				{live.kind === 'scheduler' && live.draining ? 'Draining' : 'Drain'}
			</Button>
		{/if}

		<!-- Curation overflow (Edit / Delete) is gated by `canMutate`: a platform
			 pool is read-only for everyone but a platform admin, a tenant pool by
			 workspace role. Enroll affordances above stay open (every workspace may
			 run against a shared platform pool). -->
		{#if canMutate}
			<DropdownMenu>
				<DropdownMenuTrigger
					aria-label="Pool actions"
					class="inline-flex size-8 shrink-0 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground data-[state=open]:bg-accent data-[state=open]:text-foreground"
				>
					<EllipsisVertical class="size-4" />
				</DropdownMenuTrigger>
				<DropdownMenuContent align="end">
					<DropdownMenuItem
						data-testid="pool-edit-{capacity.id}"
						onSelect={() => onedit?.(capacity.id)}
					>
						<Pencil class="size-4" />
						Edit
					</DropdownMenuItem>
					<DropdownMenuSeparator />
					<DropdownMenuItem
						variant="destructive"
						data-testid="pool-delete-{capacity.id}"
						onSelect={() => ondelete?.(capacity.id)}
					>
						<Trash2 class="size-4" />
						Delete
					</DropdownMenuItem>
				</DropdownMenuContent>
			</DropdownMenu>
		{/if}
	</div>
</div>
