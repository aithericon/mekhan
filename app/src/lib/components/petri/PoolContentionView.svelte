<!--
  PoolContentionView — live resource-pool-net contention dashboard.

  Visualises the `resource-pool-net` Petri net's current marking:
    1. Pool drain bar: free / in-use split with conservation indicator.
    2. Per-hold list: each active hold's gpu_id + grant_id.
    3. Freed units counter (cumulative since net start).

  Driven by `createPoolLiveStore` which polls
  `GET /petri/api/nets/resource-pool-net/events` every ~2 s and folds the
  event log into the marking projection.

  Contention "Waiting for resource" badge wiring (item 4):
    This component exposes the predicate but the per-instance overlay is
    wired in NodeRuntimeBadge (see "awaiting_resource" status variant).
    Wiring to a live instance graph requires M3 compiler lowering to be
    deployed (claim/grant places present in instance nets). The
    `waitingNodeIds` prop accepts a set of node ids whose `p_{id}_claim_out`
    place carries a parked token while `p_{id}_grant_inbox` is empty —
    signalling "claim sent, no grant yet". Until M3 is deployed, pass an
    empty Set and the badges will not render.
    TODO(M3): read claim_out / grant_inbox place-counts from the
    per-instance net marking and populate `waitingNodeIds` in the parent.
-->
<script lang="ts">
	import { createPoolLiveStore, POOL_NET_ID } from '$lib/stores/pool-live.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Loader2 from '@lucide/svelte/icons/loader-2';
	import AlertTriangle from '@lucide/svelte/icons/alert-triangle';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import Cpu from '@lucide/svelte/icons/cpu';
	import Eye from '@lucide/svelte/icons/eye';

	type Props = {
		/**
		 * Engine net id — defaults to `resource-pool-net`. Override to visualise
		 * a differently-named pool net (e.g. when multiple pools coexist).
		 */
		netId?: string;
		/**
		 * Optional compact mode — collapses the hold list, suitable for a
		 * sidebar panel or a thin header strip.
		 */
		compact?: boolean;
		/**
		 * Node ids that are waiting for a resource grant (M3 predicate).
		 * TODO(M3): populate from per-instance net marking once M3 compiler
		 * lowering deploys claim/grant places into instance nets.
		 */
		waitingNodeIds?: Set<string>;
	};

	let { netId = POOL_NET_ID, compact = false, waitingNodeIds = new Set<string>() }: Props =
		$props();

	const store = createPoolLiveStore(netId);

	// Percentage of pool currently in use (0–100), clamped for the progress bar.
	const usedPct = $derived(
		store.capacity > 0 ? Math.round((store.inUseCount / store.capacity) * 100) : 0
	);

	// Bar fill class — green when idle, amber when partially used, red when full.
	const barClass = $derived(
		store.inUseCount === 0
			? 'bg-green-500'
			: store.inUseCount >= store.capacity
				? 'bg-red-500'
				: 'bg-amber-500'
	);

	// Truncate long grant ids for the hold list.
	function shortId(id: string | null): string {
		if (!id) return '—';
		return id.length > 12 ? id.slice(0, 8) + '…' : id;
	}

	$effect(() => {
		store.init();
		return () => {
			store.destroy();
		};
	});
</script>

<div
	class="flex flex-col gap-3 rounded-lg border border-border bg-card p-4 text-sm"
	data-testid="pool-contention-view"
>
	<!-- Header ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── -->
	<div class="flex items-start justify-between gap-2">
		<div class="flex min-w-0 items-center gap-2">
			<Cpu class="size-4 shrink-0 text-muted-foreground" />
			<div class="flex min-w-0 flex-col leading-tight">
				<span class="font-semibold whitespace-nowrap text-foreground">Resource Pool</span>
				<span class="truncate font-mono text-sm text-muted-foreground" title={netId}>{netId}</span>
			</div>
		</div>
		<div class="flex shrink-0 items-center gap-1">
			{#if store.status === 'loading'}
				<Loader2 class="size-3.5 animate-spin text-muted-foreground" />
				<span class="whitespace-nowrap text-sm text-muted-foreground">Loading…</span>
			{:else if store.status === 'live'}
				<span class="inline-block size-2 shrink-0 rounded-full bg-green-500"></span>
				<span class="text-sm text-muted-foreground">Live</span>
			{:else if store.status === 'net-not-found'}
				<span class="whitespace-nowrap text-sm text-muted-foreground">Not deployed</span>
			{:else if store.status === 'error'}
				<XCircle class="size-3.5 text-destructive" />
				<span class="text-sm text-destructive">Error</span>
			{/if}
			<Button variant="ghost" size="icon-sm" onclick={() => store.refresh()} title="Refresh">
				<RefreshCw class="size-3.5" />
			</Button>
			<Button
				variant="ghost"
				size="icon-sm"
				href="/nets/{netId}"
				title="Open in NetWorkbench"
			>
				<Eye class="size-3.5" />
			</Button>
		</div>
	</div>

	<!-- Error / not-found notice ── ── ── ── ── ── ── ── ── ── ── ── ── -->
	{#if store.status === 'error'}
		<div class="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">
			{store.error ?? 'Unknown error'}
		</div>
	{:else if store.status === 'net-not-found'}
		<div class="rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-800">
			<p class="font-medium">Pool net not deployed</p>
			<p class="mt-0.5 text-sm">
				Deploy with:
				<code class="font-mono text-sm">
					cargo run -p aithericon-sdk --example resource_pool_net -- --deploy --net-id {netId}
				</code>
			</p>
		</div>
	{/if}

	<!-- Pool drain section ── ── ── ── ── ── ── ── ── ── ── ── ── ── ── -->
	{#if store.status === 'live' || store.status === 'loading'}
		<div class="flex flex-col gap-2">
			<div class="flex flex-col gap-0.5">
				<span class="whitespace-nowrap text-sm font-medium text-foreground">Pool utilisation</span>
				<div class="flex items-center gap-1.5 text-sm text-muted-foreground">
					{#if store.conservationOk}
						<CheckCircle2 class="size-3.5 shrink-0 text-green-600" />
						<span>{store.capacity}/{store.capacity} units accounted · {store.poolCount} free, {store.inUseCount} held</span>
					{:else if store.status === 'loading'}
						<Loader2 class="size-3.5 shrink-0 animate-spin" />
						<span>Loading…</span>
					{/if}
				</div>
			</div>

			<!-- Segmented bar: green = free, amber/red = in-use -->
			<div
				class="relative h-4 w-full overflow-hidden rounded-full bg-muted"
				title="{store.inUseCount} in use / {store.poolCount} free"
				role="meter"
				aria-label="Pool utilisation"
				aria-valuenow={store.inUseCount}
				aria-valuemin={0}
				aria-valuemax={store.capacity}
			>
				<div
					class="absolute inset-y-0 left-0 rounded-full transition-all duration-500 {barClass}"
					style="width: {usedPct}%"
				></div>
			</div>

			<!-- Count badges ── ── ── ── ── ── ── ── ── ── ── ── ── ── -->
			<div class="flex flex-wrap items-center gap-2">
				<div class="flex items-center gap-1.5 rounded-md border border-green-200 bg-green-50 px-2 py-1 text-sm font-medium text-green-700">
					<span class="tabular-nums">{store.poolCount}</span>
					<span>free</span>
				</div>
				<div
					class="flex items-center gap-1.5 rounded-md border px-2 py-1 text-sm font-medium {store.inUseCount > 0
						? 'border-amber-200 bg-amber-50 text-amber-700'
						: 'border-border bg-muted text-muted-foreground'}"
				>
					<span class="tabular-nums">{store.inUseCount}</span>
					<span>in use</span>
				</div>
				{#if store.freedCount > 0}
					<div class="flex items-center gap-1.5 rounded-md border border-slate-200 bg-slate-50 px-2 py-1 text-sm text-slate-600">
						<span class="tabular-nums">{store.freedCount}</span>
						<span>freed total</span>
					</div>
				{/if}
				{#if waitingNodeIds.size > 0}
					<div class="flex items-center gap-1.5 rounded-md border border-purple-200 bg-purple-50 px-2 py-1 text-sm font-medium text-purple-700">
						<span class="tabular-nums">{waitingNodeIds.size}</span>
						<span>waiting</span>
					</div>
				{/if}
			</div>
		</div>

		<!-- Active holds list ── ── ── ── ── ── ── ── ── ── ── ── ── ── -->
		<!-- Backend-agnostic: each hold's typed lease fields (unit_id for token
		     pools; node/gpu_uuid/alloc_id/expiry for datacenter adapters) are
		     surfaced generically from the in_use token — no field is hard-coded. -->
		{#if !compact && store.inUseCount > 0}
			<div class="flex flex-col gap-1.5">
				<span class="text-sm font-medium text-foreground">Active holds</span>
				<div class="flex flex-col divide-y divide-border rounded-md border border-border">
					{#each store.holds as hold (hold.tokenId)}
						<div class="flex flex-col gap-1 px-3 py-2 text-sm" data-testid="hold-row">
							<div class="flex items-center justify-between gap-2">
								<div class="flex min-w-0 items-center gap-2">
									<Cpu class="size-3.5 shrink-0 text-amber-600" />
									<span class="truncate font-mono text-sm text-muted-foreground" title="grant id: {hold.grantId ?? '—'}">
										grant {shortId(hold.grantId)}
									</span>
								</div>
								<Badge
									class="bg-amber-100 text-sm font-normal text-amber-700"
									variant="secondary"
								>
									held
								</Badge>
							</div>
							{#if Object.keys(hold.fields).length > 0}
								<div class="flex flex-wrap gap-x-3 gap-y-0.5 pl-5">
									{#each Object.entries(hold.fields) as [key, value] (key)}
										<span class="text-sm text-muted-foreground" title={`${key}: ${value}`}>
											<span class="text-muted-foreground/70">{key}</span>
											<span class="font-mono font-medium text-foreground">{value}</span>
										</span>
									{/each}
								</div>
							{/if}
						</div>
					{/each}
				</div>
			</div>
		{:else if !compact && store.inUseCount === 0 && store.capacity > 0}
			<div class="flex items-center justify-center gap-2 rounded-md border border-green-200 bg-green-50 py-3 text-sm text-green-700">
				<CheckCircle2 class="size-4" />
				<span>All {store.capacity} units free — no active holds</span>
			</div>
		{/if}

		<!-- Event count (debug / last-updated indicator) ── ── ── ── ── -->
		{#if store.eventCount > 0}
			<div class="text-right text-sm text-muted-foreground">
				{store.eventCount} events projected
			</div>
		{/if}
	{/if}
</div>
