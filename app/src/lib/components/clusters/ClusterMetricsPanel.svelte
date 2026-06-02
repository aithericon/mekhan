<script lang="ts">
	import type { ClusterMetrics } from '$lib/api/clusters';

	type Props = {
		metrics: ClusterMetrics | null;
		loading: boolean;
	};

	let { metrics, loading }: Props = $props();

	/** Format seconds -> hours with 3 decimal places, or em-dash when zero. */
	function hoursFrom(seconds: number): string {
		if (seconds === 0) return '0';
		return (seconds / 3600).toFixed(3);
	}

	/** Format milliseconds to a human-readable string. */
	function fmtMs(ms: number | null | undefined): string {
		if (ms === null || ms === undefined) return '—';
		if (ms < 1000) return `${Math.round(ms)}ms`;
		return `${(ms / 1000).toFixed(2)}s`;
	}

	/** Success-rate percent string, or em-dash when no leases yet. */
	const successRateLabel = $derived(
		metrics
			? metrics.lease_count === 0
				? '—'
				: `${(metrics.success_rate * 100).toFixed(1)}%`
			: '—'
	);

	/** Window label derived from the ClusterMetrics timestamps. */
	const windowLabel = $derived(
		metrics
			? (() => {
					const start = new Date(metrics.window_start);
					const end = new Date(metrics.window_end);
					const diffMs = end.getTime() - start.getTime();
					const diffH = diffMs / 3_600_000;
					if (diffH >= 24 * 0.99) {
						return `${Math.round(diffH / 24)}d window`;
					}
					return `${Math.round(diffH)}h window`;
				})()
			: ''
	);
</script>

<section class="space-y-3">
	<div class="flex items-center justify-between">
		<h2 class="text-sm font-semibold tracking-tight text-foreground">Metrics</h2>
		{#if metrics}
			<span class="text-sm text-muted-foreground">{windowLabel}</span>
		{/if}
	</div>

	{#if loading && !metrics}
		<p class="text-sm text-muted-foreground">Loading metrics...</p>
	{:else if metrics}
		<!-- KPI card grid -->
		<div class="grid grid-cols-2 gap-2 sm:grid-cols-4">
			<!-- Active leases -->
			<div class="rounded-md border border-border bg-card px-3 py-2.5">
				<div class="text-sm text-muted-foreground">Active leases</div>
				<div class="mt-0.5 text-lg font-semibold tabular-nums text-foreground">
					{metrics.active_lease_count}
				</div>
				<div class="mt-0.5 text-sm text-muted-foreground">live</div>
			</div>

			<!-- Success rate -->
			<div class="rounded-md border border-border bg-card px-3 py-2.5">
				<div class="text-sm text-muted-foreground">Success rate</div>
				<div class="mt-0.5 text-lg font-semibold tabular-nums
					{metrics.lease_count > 0 && metrics.success_rate < 0.8
						? 'text-rose-600'
						: metrics.lease_count > 0 && metrics.success_rate >= 0.95
							? 'text-emerald-600'
							: 'text-foreground'}">
					{successRateLabel}
				</div>
				<div class="mt-0.5 text-sm text-muted-foreground">
					{metrics.released_count}/{metrics.lease_count} released
				</div>
			</div>

			<!-- CPU-hours -->
			<div class="rounded-md border border-border bg-card px-3 py-2.5">
				<div class="text-sm text-muted-foreground">CPU-hours</div>
				<div class="mt-0.5 text-lg font-semibold tabular-nums text-foreground">
					{hoursFrom(metrics.cpu_seconds_total)}
				</div>
				<div class="mt-0.5 text-sm text-muted-foreground">
					{metrics.held_cpu_seconds > 0 ? `${hoursFrom(metrics.held_cpu_seconds)} held` : 'none held'}
				</div>
			</div>

			<!-- GPU-hours -->
			<div class="rounded-md border border-border bg-card px-3 py-2.5">
				<div class="text-sm text-muted-foreground">GPU-hours</div>
				<div class="mt-0.5 text-lg font-semibold tabular-nums text-foreground">
					{hoursFrom(metrics.gpu_seconds_total)}
				</div>
				<div class="mt-0.5 text-sm text-muted-foreground">
					{metrics.held_gpu_seconds > 0 ? `${hoursFrom(metrics.held_gpu_seconds)} held` : 'none held'}
				</div>
			</div>
		</div>

		<!-- Lease counts + queue-wait row -->
		<div class="grid grid-cols-2 gap-2 sm:grid-cols-4">
			<div class="rounded-md border border-border bg-card px-3 py-2.5">
				<div class="text-sm text-muted-foreground">Leases (window)</div>
				<div class="mt-0.5 font-semibold tabular-nums text-foreground">{metrics.lease_count}</div>
				<div class="mt-0.5 flex gap-2 text-sm">
					<span class="text-emerald-600">{metrics.released_count} ok</span>
					{#if metrics.failed_count > 0}
						<span class="text-rose-600">{metrics.failed_count} failed</span>
					{/if}
				</div>
			</div>

			<div class="rounded-md border border-border bg-card px-3 py-2.5">
				<div class="text-sm text-muted-foreground">Queue wait p50</div>
				<div class="mt-0.5 font-semibold tabular-nums text-foreground">
					{fmtMs(metrics.queue_wait_p50_ms)}
				</div>
			</div>

			<div class="rounded-md border border-border bg-card px-3 py-2.5">
				<div class="text-sm text-muted-foreground">Queue wait p95</div>
				<div class="mt-0.5 font-semibold tabular-nums text-foreground">
					{fmtMs(metrics.queue_wait_p95_ms)}
				</div>
			</div>

			<div class="rounded-md border border-border bg-card px-3 py-2.5">
				<div class="text-sm text-muted-foreground">Queue wait p99</div>
				<div class="mt-0.5 font-semibold tabular-nums text-foreground">
					{fmtMs(metrics.queue_wait_p99_ms)}
				</div>
			</div>
		</div>
	{:else}
		<p class="text-sm text-muted-foreground">No metrics available.</p>
	{/if}
</section>
