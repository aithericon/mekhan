<script lang="ts">
	// ROUTER tab — REAL self-hosted-inference telemetry (inference bypasses the
	// engine net; the HTTP router meters directly). Three reads, no Prometheus
	// dependency for this page:
	//   1. LIVE gauges  ← GET /api/v1/inference/router-live — a point-in-time
	//      proxy of the router's /metrics: per-replica admission, per-model
	//      in-flight + starvation, global counters. Polled ~5s.
	//   2. HISTORY charts ← GET /api/v1/inference/timeseries — per-model
	//      throughput / latency / error-rate, time-bucketed over the durable
	//      `inference_request_log` ledger (TimescaleDB). Polled ~20s.
	//   3. Audit LEDGER ← GET /api/v1/inference/requests — one row per request,
	//      the metering / GDPR record. Newest first.
	// A real Prometheus scraping the router is an optional OPS layer
	// (`just dev up-prometheus`) for Grafana — not what this page reads.
	import { onDestroy } from 'svelte';
	import InferenceAuditTable from '$lib/components/fleet/InferenceAuditTable.svelte';
	import RouterLiveGauges from '$lib/components/fleet/RouterLiveGauges.svelte';
	import InferenceTimeseriesChart from '$lib/components/fleet/InferenceTimeseriesChart.svelte';
	import { Button } from '$lib/components/ui/button';
	import {
		getRouterLive,
		listInferenceTimeseries,
		type RouterLiveMetrics,
		type InferenceTimeseriesPoint
	} from '$lib/api/inference';
	import {
		TIMESERIES_METRICS,
		WINDOW_CHOICES,
		pivotTimeseries,
		type WindowChoice
	} from '$lib/components/fleet/inference-telemetry';

	let live = $state<RouterLiveMetrics | null>(null);
	let points = $state<InferenceTimeseriesPoint[]>([]);
	let tsError = $state<string | null>(null);
	let tsLoading = $state(true);

	let win = $state<WindowChoice>(WINDOW_CHOICES[1]); // default 1h
	let metricKey = $state('requests');

	const metric = $derived(
		TIMESERIES_METRICS.find((m) => m.key === metricKey) ?? TIMESERIES_METRICS[0]
	);
	const pivot = $derived(pivotTimeseries(points, metric.valueOf, metric.missing));

	async function pollLive() {
		try {
			live = await getRouterLive();
		} catch {
			live = null;
		}
	}

	async function loadTs() {
		try {
			points = await listInferenceTimeseries({
				windowSecs: win.windowSecs,
				bucketSecs: win.bucketSecs
			});
			tsError = null;
		} catch (e) {
			tsError = e instanceof Error ? e.message : 'Failed to load timeseries';
		} finally {
			tsLoading = false;
		}
	}

	// Live gauges: fast poll (point-in-time operational state).
	$effect(() => {
		void pollLive();
		const t = setInterval(() => void pollLive(), 5000);
		return () => clearInterval(t);
	});

	// Timeseries: reload on window change + slow background refresh.
	$effect(() => {
		void win; // re-run (and refetch) when the window changes
		void loadTs();
		const t = setInterval(() => void loadTs(), 20000);
		return () => clearInterval(t);
	});

	onDestroy(() => {});
</script>

<div class="space-y-6" data-testid="models-router">
	<!-- Live operational gauges (point-in-time, proxied from the router). -->
	<section class="space-y-3" data-testid="router-telemetry">
		<div class="flex items-baseline gap-3">
			<h2 class="text-base font-semibold tracking-tight text-foreground">Live</h2>
			<span class="text-sm text-muted-foreground"
				>router admission, per-model demand &amp; global counters — right now</span
			>
		</div>
		<RouterLiveGauges {live} />
	</section>

	<!-- Historical per-model charts over the durable ledger. -->
	<section class="space-y-3">
		<div class="flex flex-wrap items-baseline justify-between gap-3">
			<div class="flex items-baseline gap-3">
				<h2 class="text-base font-semibold tracking-tight text-foreground">Over time</h2>
				<span class="text-sm text-muted-foreground"
					>per-model throughput, latency &amp; error rate — from the metering ledger</span
				>
			</div>
			<div class="flex items-center gap-1 rounded-lg border border-border bg-card p-0.5">
				{#each WINDOW_CHOICES as w (w.label)}
					<Button
						variant={win.label === w.label ? 'default' : 'ghost'}
						size="sm"
						class="h-7 px-2.5 text-xs"
						onclick={() => (win = w)}
					>
						{w.label}
					</Button>
				{/each}
			</div>
		</div>

		<div class="rounded-xl border border-border bg-card p-4">
			<div class="mb-3 flex flex-wrap gap-1.5">
				{#each TIMESERIES_METRICS as m (m.key)}
					<Button
						variant={metric.key === m.key ? 'secondary' : 'ghost'}
						size="sm"
						class="h-7 px-2.5 text-xs"
						onclick={() => (metricKey = m.key)}
						data-testid="ts-metric-{m.key}"
					>
						{m.label}
					</Button>
				{/each}
			</div>

			{#if tsError}
				<div
					class="rounded-lg border border-rose-200 bg-rose-50 px-3 py-2 text-sm text-rose-700 dark:border-rose-900/50 dark:bg-rose-950/30 dark:text-rose-300"
				>
					{tsError}
				</div>
			{:else if !tsLoading && points.length === 0}
				<div class="flex h-[240px] items-center justify-center text-sm text-muted-foreground">
					No inference requests in this window yet — run a model-pool workflow node or hit the router,
					then the throughput, latency and error-rate series fill in here.
				</div>
			{:else}
				<InferenceTimeseriesChart
					categories={pivot.categories}
					series={pivot.series}
					unit={metric.unit}
					connectNulls={metric.missing === 'zero'}
				/>
			{/if}
		</div>
		<p class="text-xs text-muted-foreground/80">
			These charts read the durable ledger directly — no Prometheus required. For Grafana dashboards
			or to query the router from a workflow, run the optional ops scraper:
			<code class="rounded bg-muted px-1 py-px font-mono text-foreground/80">just dev up-prometheus</code
			>.
		</p>
	</section>

	<!-- Durable per-request audit ledger (metering / GDPR record). -->
	<section class="space-y-3">
		<div class="flex items-baseline gap-3">
			<h2 class="text-base font-semibold tracking-tight text-foreground">Audit ledger</h2>
			<span class="text-sm text-muted-foreground">
				inference audit — newest first (durable per-request metering / GDPR record)
			</span>
		</div>
		<InferenceAuditTable />
	</section>
</div>
