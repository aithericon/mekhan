<script lang="ts">
	// Per-capacity detail. Resolves the `capacity` by its id (the CapacityCard
	// "Detail" / "Enroll here" deep-link `/fleet/{id}`), then DISPATCHES on its
	// dispatch backend — mirroring the Control Plane's backend sections:
	//
	//   presence → [Runners | Interfaces] sub-tabs SCOPED to that runner group. The
	//              Runners tab is the consolidated roster (RunnerList) — the old
	//              Board/Roster split was two views of the same data; the coverage
	//              strip + pool-net link + live backends are folded into the roster.
	//   queue    → a Workers view: the enrolled workers whose `group` is this
	//              capacity's path, each a FleetCard (shared with WorkerPoolBoard).
	//   tokens   → a Holders view: the seeded/in-use gauge + the live grant holders.
	//   scheduler → NOT detailed here (those cards link to /clusters/{id}); if one
	//              somehow lands here, we just link out.
	import { page } from '$app/state';
	import * as Tabs from '$lib/components/ui/tabs';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import UserPlus from '@lucide/svelte/icons/user-plus';
	import Cpu from '@lucide/svelte/icons/cpu';
	import KeyRound from '@lucide/svelte/icons/key-round';
	import RunnerList from '$lib/components/fleet/RunnerList.svelte';
	import InterfacesCatalog from '$lib/components/fleet/InterfacesCatalog.svelte';
	import EnrollSheet from '$lib/components/fleet/EnrollSheet.svelte';
	import FleetCard from '$lib/components/fleet/FleetCard.svelte';
	import FleetEmpty from '$lib/components/fleet/FleetEmpty.svelte';
	import { fmtMsAgo, fmtDate } from '$lib/components/fleet/format';
	import { listCapacities, type CapacitySummary } from '$lib/api/capacities';
	import { listWorkers, type WorkerSummary } from '$lib/api/workers';

	const resourceId = $derived(page.params.id ?? '');

	let capacity = $state<CapacitySummary | null>(null);
	let error = $state<string | null>(null);
	let activeTab = $state<'runners' | 'interfaces'>('runners');
	let enrollOpen = $state(false);

	// Workers in this queue group (loaded only for queue-backed capacities).
	let workers = $state<WorkerSummary[]>([]);

	/** The capacity alias (`path`) every scoped child view binds to. */
	const groupAlias = $derived(capacity?.path ?? null);
	const name = $derived(capacity?.display_name || capacity?.path || resourceId);
	const backend = $derived(capacity?.backend ?? null);

	/** Workers enrolled into THIS queue group (by alias). */
	const groupWorkers = $derived(
		groupAlias ? workers.filter((w) => w.group === groupAlias) : []
	);

	/** A worker's advertised backends — `WorkerSummary.backends` is wire `unknown`. */
	function workerBackends(w: WorkerSummary): string[] {
		return Array.isArray(w.backends) ? (w.backends as string[]) : [];
	}

	/** Freshness line off `last_seen_at` (ISO) — "live · 3s ago" / "never seen". */
	function workerMeta(w: WorkerSummary): string {
		const bs = workerBackends(w);
		const head = `${bs.length} backend${bs.length === 1 ? '' : 's'}`;
		if (!w.last_seen_at) return `${head} · never seen`;
		const ms = Date.now() - new Date(w.last_seen_at).getTime();
		return Number.isNaN(ms) ? `${head} · ${w.status}` : `${head} · ${fmtMsAgo(ms)}`;
	}

	async function load() {
		try {
			const all = await listCapacities();
			capacity = all.find((c) => c.id === resourceId) ?? null;
			error = capacity ? null : 'Capacity not found.';
			if (capacity?.backend === 'queue') {
				const paged = await listWorkers();
				workers = paged.items;
			} else {
				workers = [];
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load capacity';
		}
	}

	$effect(() => {
		void resourceId;
		void load();
	});
</script>

<svelte:head><title>{name} | Control Plane | Mekhan</title></svelte:head>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-6xl px-6 py-8 animate-rise">
		<a
			href="/fleet"
			class="mb-3 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
		>
			<ChevronLeft class="size-4" /> Control Plane
		</a>

		<div class="mb-4 flex items-start justify-between gap-3">
			<div>
				<h1 class="text-lg font-semibold tracking-tight" data-testid="group-detail-title">{name}</h1>
				<div class="mt-1 flex items-center gap-2 text-sm">
					<Badge variant="secondary">{backend ?? 'capacity'}</Badge>
					{#if capacity}
						<span class="font-mono text-xs text-muted-foreground">{capacity.path}</span>
						{#if capacity.live.kind === 'presence'}
							<span class="text-xs text-muted-foreground tabular-nums">
								{capacity.live.online}/{capacity.live.total} online
							</span>
						{:else if capacity.live.kind === 'queue'}
							<span class="text-xs text-muted-foreground tabular-nums">
								{capacity.live.online}/{capacity.live.enrolled} online
							</span>
						{:else if capacity.live.kind === 'tokens'}
							<span class="text-xs text-muted-foreground tabular-nums">
								{capacity.live.in_use}/{capacity.live.seeded} in use
							</span>
						{/if}
					{:else}
						<span class="font-mono text-xs text-muted-foreground">{resourceId}</span>
					{/if}
				</div>
			</div>
			{#if groupAlias && backend === 'queue'}
				<!-- Presence enroll lives in the runner-cards header row (RunnerList);
					 the worker roster has no such row, so it keeps the header action. -->
				<Button
					variant="outline"
					size="sm"
					class="gap-1.5"
					onclick={() => (enrollOpen = true)}
					data-testid="group-enroll-here"
				>
					<UserPlus class="size-4" />
					Enroll worker here
				</Button>
			{/if}
		</div>

		{#if error}
			<div
				class="mb-3 rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
			>
				{error}
			</div>
		{/if}

		{#if backend === 'presence' && groupAlias}
			<!-- PRESENCE — one consolidated Runners roster + Interfaces, scoped to the
				 runner group. -->
			<Tabs.Root
				value={activeTab}
				onValueChange={(v) => (activeTab = v as typeof activeTab)}
				class="mb-5"
			>
				<Tabs.List>
					<Tabs.Trigger value="runners" data-testid="group-tab-runners">Runners</Tabs.Trigger>
					<Tabs.Trigger value="interfaces" data-testid="group-tab-interfaces">Interfaces</Tabs.Trigger>
				</Tabs.List>
			</Tabs.Root>

			{#if activeTab === 'runners'}
				<RunnerList group={groupAlias} roster onenroll={() => (enrollOpen = true)} />
			{:else}
				<InterfacesCatalog group={groupAlias} />
			{/if}
		{:else if backend === 'queue'}
			<!-- QUEUE — the workers enrolled into this group. -->
			<div class="space-y-2" data-testid="group-workers">
				<h4 class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
					Workers
				</h4>
				{#if groupWorkers.length === 0}
					<FleetEmpty
						message="No workers in this group."
						hint="Enroll a worker — it competes for this group's queued jobs."
					>
						{#snippet icon()}<Cpu class="size-10 text-muted-foreground/40" />{/snippet}
					</FleetEmpty>
				{:else}
					<div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
						{#each groupWorkers as w (w.id)}
							<FleetCard
								title={w.name}
								tone={w.status === 'online' ? 'live' : 'idle'}
								meta={workerMeta(w)}
								backends={workerBackends(w)}
								testid="group-worker-{w.id}"
							>
								{#snippet tooltip()}
									<p class="font-mono text-sm">{w.name}</p>
									<p class="text-sm">Status: {w.status}</p>
									{#if workerBackends(w).length > 0}
										<p class="text-sm">Serves: {workerBackends(w).join(', ')}</p>
									{/if}
								{/snippet}
							</FleetCard>
						{/each}
					</div>
				{/if}
			</div>
		{:else if backend === 'tokens'}
			<!-- TOKENS — the seeded/in-use gauge + the live grant holders. -->
			{#if capacity && capacity.live.kind === 'tokens'}
				{@const live = capacity.live}
				<div class="space-y-4" data-testid="group-holders">
					<div class="flex items-center gap-3 rounded-lg border border-border bg-card px-4 py-3">
						<KeyRound class="size-5 text-muted-foreground" />
						<div>
							<p class="text-sm font-medium text-foreground tabular-nums">
								{live.in_use}/{live.seeded} in use
							</p>
							<p class="text-xs text-muted-foreground">
								{live.seeded - live.in_use} token{live.seeded - live.in_use === 1 ? '' : 's'} free
							</p>
						</div>
					</div>

					<div class="space-y-2">
						<h4 class="text-sm font-medium uppercase tracking-wide text-muted-foreground">
							Holders
						</h4>
						{#if live.holders.length === 0}
							<FleetEmpty message="No tokens held.">
								{#snippet icon()}<KeyRound class="size-10 text-muted-foreground/40" />{/snippet}
							</FleetEmpty>
						{:else}
							<div class="overflow-hidden rounded-lg border border-border">
								<table class="w-full text-sm">
									<thead class="bg-muted/50 text-muted-foreground">
										<tr>
											<th class="px-3 py-2 text-left font-medium">Instance</th>
											<th class="px-3 py-2 text-left font-medium">Since</th>
										</tr>
									</thead>
									<tbody>
										{#each live.holders as h, i (h.instance_id ?? i)}
											<tr class="border-t border-border">
												<td class="px-3 py-2 font-mono text-xs text-foreground">
													{h.instance_id ?? '—'}
												</td>
												<td class="px-3 py-2 text-muted-foreground">{fmtDate(h.since)}</td>
											</tr>
										{/each}
									</tbody>
								</table>
							</div>
						{/if}
					</div>
				</div>
			{/if}
		{:else if backend === 'scheduler'}
			<!-- SCHEDULER — detailed on the cluster page, not here. -->
			<div class="rounded-lg border border-border bg-card px-4 py-3 text-sm">
				Scheduler capacities are managed on the cluster page.
				<a class="font-medium text-foreground underline" href="/clusters/{resourceId}">
					Open cluster →
				</a>
			</div>
		{/if}
	</div>
</div>

<EnrollSheet
	bind:open={enrollOpen}
	mode={backend === 'queue' ? 'worker' : 'runner'}
	group={groupAlias}
	onenrolled={() => void load()}
/>
