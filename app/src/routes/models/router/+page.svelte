<script lang="ts">
	// ROUTER tab — two read-only views over self-hosted inference (which bypasses
	// the engine net; the HTTP router meters directly):
	//   1. The per-model TELEMETRY pointer. Throughput / rate / latency "over time"
	//      live in Prometheus — the router exposes them at GET /metrics and Grafana
	//      owns the dashboards. We don't re-aggregate a time-series here; we just
	//      name the series so an operator knows what to graph.
	//   2. The durable audit LEDGER (GET /api/v1/inference/requests) — one row per
	//      request, the metering / GDPR processing record (who served what, token
	//      counts, outcome). Per-request, newest first.
	import InferenceAuditTable from '$lib/components/fleet/InferenceAuditTable.svelte';
	import LineChart from '@lucide/svelte/icons/chart-line';

	// The per-model series the router publishes on its Prometheus /metrics endpoint
	// (router/src/metrics.rs). Labelled by `model` (and `status` / `le` where noted).
	const METRICS: { name: string; labels?: string; help: string }[] = [
		{ name: 'inference_router_model_inflight', help: 'In-flight requests per model (queue depth / current load).' },
		{ name: 'inference_router_model_requests_total', labels: 'status', help: 'Terminal requests per model by status — request rate + error rate.' },
		{ name: 'inference_router_model_prompt_tokens_total', help: 'Prompt (input) tokens per model.' },
		{ name: 'inference_router_model_completion_tokens_total', help: 'Completion (output) tokens per model.' },
		{ name: 'inference_router_request_duration_seconds', labels: 'le', help: 'Request-duration histogram per model (latency p50/p95/p99).' },
		{ name: 'inference_router_model_starved_total', help: 'Requests that found no live/un-saturated replica (unmet demand).' }
	];
</script>

<div class="space-y-6" data-testid="models-router">
	<!-- Telemetry pointer: history lives in Prometheus/Grafana, not in mekhan. -->
	<section class="space-y-3" data-testid="router-telemetry">
		<div class="flex items-baseline gap-3">
			<h2 class="text-base font-semibold tracking-tight text-foreground">Telemetry</h2>
			<span class="text-sm text-muted-foreground">per-model throughput, rate &amp; latency — over time</span>
		</div>
		<div class="rounded-xl border border-border bg-card p-4">
			<div class="flex items-start gap-3">
				<LineChart class="mt-0.5 size-5 shrink-0 text-muted-foreground/70" />
				<div class="space-y-1 text-sm text-muted-foreground">
					<p>
						The router exposes per-model time-series on its Prometheus endpoint
						(<code class="rounded bg-muted px-1 py-px font-mono text-foreground/80">GET /metrics</code>).
						Scrape it with Prometheus and chart <span class="text-foreground/90">tokens in/out, request &amp; error rate, queue depth, and latency percentiles per model</span> in Grafana — that's where the over-time view lives.
					</p>
					<p class="text-muted-foreground/80">
						Inference never crosses the engine net, so these are emitted by the router directly (control-plane / data-plane separation).
					</p>
				</div>
			</div>
			<ul class="mt-3 grid gap-1.5 border-t border-border/50 pt-3 sm:grid-cols-2">
				{#each METRICS as m (m.name)}
					<li class="text-sm">
						<code class="font-mono text-foreground/90">
							{m.name}<span class="text-muted-foreground/60"
								>{'{'}model{m.labels ? `,${m.labels}` : ''}{'}'}</span
							>
						</code>
						<span class="block text-muted-foreground">{m.help}</span>
					</li>
				{/each}
			</ul>
		</div>
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
