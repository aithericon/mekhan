<script lang="ts">
	import ClusterList from '$lib/components/clusters/ClusterList.svelte';
	import { fleetMetrics, type ClusterMetrics, type FleetMetrics } from '$lib/api/clusters';

	let fleet = $state<FleetMetrics | null>(null);
	let fleetLoading = $state(true);

	async function loadFleet() {
		try {
			fleet = await fleetMetrics('24h');
		} catch {
			// best-effort — the list remains useful without metrics
		} finally {
			fleetLoading = false;
		}
	}

	$effect(() => {
		loadFleet();
		const t = setInterval(loadFleet, 4000);
		return () => clearInterval(t);
	});

	/** Build a map from cluster_id -> ClusterMetrics for the per-row lookup. */
	const metricsById = $derived(
		fleet
			? Object.fromEntries(fleet.clusters.map((m: ClusterMetrics) => [m.cluster_id, m]))
			: {}
	);

	function pct(rate: number): string {
		return `${(rate * 100).toFixed(1)}%`;
	}

	function hoursFrom(seconds: number): string {
		if (seconds === 0) return '0';
		return (seconds / 3600).toFixed(1);
	}
</script>

<svelte:head><title>Clusters | Mekhan</title></svelte:head>

<div class="h-full overflow-y-auto" data-testid="clusters-page">
	<div class="mx-auto max-w-6xl px-6 py-8 animate-rise space-y-8">

		<!-- Fleet summary bar -->
		{#if fleet && !fleetLoading}
			{@const ft = fleet.fleet_total}
			<div class="rounded-md border border-border/60 bg-muted/30 px-4 py-3">
				<div class="mb-2 text-sm font-medium text-muted-foreground">Fleet · 24h</div>
				<div class="flex flex-wrap gap-x-8 gap-y-2 text-sm">
					<div>
						<span class="text-muted-foreground">Active leases</span>
						<span class="ml-2 font-semibold tabular-nums text-foreground">{ft.active_lease_count}</span>
					</div>
					<div>
						<span class="text-muted-foreground">Success rate</span>
						<span class="ml-2 font-semibold tabular-nums
							{ft.lease_count > 0 && ft.success_rate < 0.8 ? 'text-rose-600' : ft.lease_count > 0 && ft.success_rate >= 0.95 ? 'text-emerald-600' : 'text-foreground'}">
							{ft.lease_count === 0 ? '—' : pct(ft.success_rate)}
						</span>
					</div>
					<div>
						<span class="text-muted-foreground">CPU-hours</span>
						<span class="ml-2 font-semibold tabular-nums text-foreground">{hoursFrom(ft.cpu_seconds_total)}</span>
					</div>
					<div>
						<span class="text-muted-foreground">GPU-hours</span>
						<span class="ml-2 font-semibold tabular-nums text-foreground">{hoursFrom(ft.gpu_seconds_total)}</span>
					</div>
					<div>
						<span class="text-muted-foreground">Leases</span>
						<span class="ml-2 font-semibold tabular-nums text-foreground">{ft.lease_count}</span>
					</div>
				</div>
			</div>
		{/if}

		<!-- Cluster list (self-contained; owns its own live-state polling) -->
		<ClusterList {metricsById} />

	</div>
</div>
