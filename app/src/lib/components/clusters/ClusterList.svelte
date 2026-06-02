<script lang="ts">
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import Activity from '@lucide/svelte/icons/activity';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';
	import Server from '@lucide/svelte/icons/server';
	import {
		listClusters,
		reconnectCluster,
		drainCluster,
		type ClusterSummary,
		type ClusterMetrics
	} from '$lib/api/clusters';

	type Props = {
		/** Per-cluster metrics map (cluster_id -> ClusterMetrics) injected from the fleet
		 *  page. When absent the metric columns are hidden so the component degrades
		 *  gracefully when used standalone. */
		metricsById?: Record<string, ClusterMetrics>;
	};

	let { metricsById = {} }: Props = $props();

	let clusters = $state<ClusterSummary[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let busyId = $state<string | null>(null);

	const hasMetrics = $derived(Object.keys(metricsById).length > 0);

	async function load(spin = false) {
		if (spin) loading = true;
		try {
			clusters = await listClusters();
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load clusters';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		load(true);
		const t = setInterval(() => load(false), 4000);
		return () => clearInterval(t);
	});

	async function act(id: string, fn: (id: string) => Promise<unknown>) {
		busyId = id;
		try {
			await fn(id);
			await load(false);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Action failed';
		} finally {
			busyId = null;
		}
	}

	function flavorClass(f: string): string {
		if (f === 'slurm') return 'bg-sky-500/15 text-sky-700 dark:text-sky-300';
		if (f === 'nomad') return 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-300';
		if (f === 'http') return 'bg-violet-500/15 text-violet-700 dark:text-violet-300';
		return 'bg-muted text-muted-foreground';
	}
	function watcherClass(s: string): string {
		if (s === 'streaming') return 'bg-emerald-500/15 text-emerald-700 dark:text-emerald-300';
		if (s === 'reconnecting') return 'bg-amber-500/15 text-amber-700 dark:text-amber-300';
		if (s === 'idle') return 'bg-muted text-muted-foreground';
		return 'bg-rose-500/15 text-rose-700 dark:text-rose-300'; // stopped / no_watcher
	}
	const name = (c: ClusterSummary) => c.display_name || c.resource_path || c.resource_id;

	function pct(rate: number, leaseCount: number): string {
		if (leaseCount === 0) return '—';
		return `${(rate * 100).toFixed(1)}%`;
	}
	function hoursFrom(seconds: number): string {
		if (seconds === 0) return '0';
		return (seconds / 3600).toFixed(1);
	}
</script>

<div class="space-y-4">
	<div class="flex items-center justify-between">
		<div>
			<h1 class="text-lg font-semibold tracking-tight">Clusters</h1>
			<p class="text-sm text-muted-foreground">
				Datacenter resources, lease state, and pool contention.
			</p>
		</div>
		<Button variant="outline" size="sm" onclick={() => load(true)} disabled={loading}>
			<RefreshCw class="mr-1.5 size-4 {loading ? 'animate-spin' : ''}" /> Refresh
		</Button>
	</div>

	{#if error}
		<div class="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
			{error}
		</div>
	{/if}

	{#if loading && clusters.length === 0}
		<p class="text-sm text-muted-foreground">Loading clusters...</p>
	{:else if clusters.length === 0}
		<div class="rounded-md border border-dashed border-border/60 px-4 py-10 text-center">
			<Server class="mx-auto size-8 text-muted-foreground/50" />
			<p class="mt-2 text-sm font-medium">No clusters registered</p>
			<p class="text-sm text-muted-foreground">
				Add a <code class="font-mono text-xs">datacenter</code> resource under
				<a href="/resources" class="underline underline-offset-2">Resources</a> to register a Slurm
				or Nomad cluster.
			</p>
		</div>
	{:else}
		<div class="overflow-hidden rounded-md border border-border/60">
			<table class="w-full text-sm">
				<thead class="bg-muted/40 text-left text-xs text-muted-foreground">
					<tr>
						<th class="px-3 py-2 font-medium">Cluster</th>
						<th class="px-3 py-2 font-medium">Flavor</th>
						<th class="px-3 py-2 font-medium">Watcher</th>
						<th class="px-3 py-2 font-medium">Health</th>
						<th class="px-3 py-2 text-right font-medium">Leases</th>
						{#if hasMetrics}
							<th class="px-3 py-2 text-right font-medium">Success</th>
							<th class="px-3 py-2 text-right font-medium">GPU-h</th>
						{/if}
						<th class="px-3 py-2 font-medium"></th>
					</tr>
				</thead>
				<tbody class="divide-y divide-border/60">
					{#each clusters as c (c.resource_id)}
						{@const cm = metricsById[c.resource_id]}
						<tr class="hover:bg-muted/30">
							<td class="px-3 py-2">
								<div class="font-medium">{name(c)}</div>
								{#if c.resource_path && c.resource_path !== name(c)}
									<div class="font-mono text-xs text-muted-foreground">{c.resource_path}</div>
								{/if}
								{#if c.last_error}
									<div class="mt-0.5 truncate text-xs text-rose-600" title={c.last_error}>
										{c.last_error}
									</div>
								{/if}
							</td>
							<td class="px-3 py-2">
								<Badge variant="secondary" class={flavorClass(c.flavor)}>{c.flavor}</Badge>
							</td>
							<td class="px-3 py-2">
								<Badge variant="secondary" class={watcherClass(c.watcher_state)}>
									{c.watcher_state}
								</Badge>
								{#if c.draining}
									<Badge variant="outline" class="ml-1">draining</Badge>
								{/if}
							</td>
							<td class="px-3 py-2 text-muted-foreground">{c.connection_health}</td>
							<td class="px-3 py-2 text-right font-mono tabular-nums">
								{#if c.active_lease_count > 0}
									<span class="inline-flex items-center gap-1 text-emerald-600">
										<Activity class="size-3.5" />{c.active_lease_count}
									</span>
								{:else}
									<span class="text-muted-foreground">0</span>
								{/if}
							</td>
							{#if hasMetrics}
								<td class="px-3 py-2 text-right font-mono text-sm tabular-nums">
									{#if cm}
										<span class={cm.lease_count > 0 && cm.success_rate < 0.8 ? 'text-rose-600' : cm.lease_count > 0 && cm.success_rate >= 0.95 ? 'text-emerald-600' : 'text-foreground'}>
											{pct(cm.success_rate, cm.lease_count)}
										</span>
									{:else}
										<span class="text-muted-foreground">—</span>
									{/if}
								</td>
								<td class="px-3 py-2 text-right font-mono text-sm tabular-nums text-muted-foreground">
									{cm ? hoursFrom(cm.gpu_seconds_total) : '—'}
								</td>
							{/if}
							<td class="px-3 py-2">
								<div class="flex items-center justify-end gap-1.5">
									{#if c.resource_id !== '_env'}
										<Button
											variant="ghost"
											size="sm"
											class="gap-1.5"
											href="/clusters/{c.resource_id}"
											title="Live lease contention"
										>
											<Activity class="size-3.5" />
											Contention
										</Button>
										<Button
											variant="ghost"
											size="sm"
											disabled={busyId === c.resource_id}
											onclick={() => act(c.resource_id, reconnectCluster)}
											title="Drop the watcher/allocator session; next lease rebuilds it"
											aria-label="Reconnect cluster"
										>
											<RotateCcw class="size-4" />
										</Button>
										<Button
											variant="ghost"
											size="sm"
											disabled={busyId === c.resource_id || c.draining}
											onclick={() => act(c.resource_id, drainCluster)}
											title="Refuse new leases while held ones finish"
										>
											Drain
										</Button>
									{:else}
										<span class="text-xs text-muted-foreground">env bootstrap</span>
									{/if}
								</div>
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	{/if}
</div>
