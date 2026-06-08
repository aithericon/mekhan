<script lang="ts">
	// Point-in-time router operational gauges (GET /api/v1/inference/router-live).
	// These are the LIVE state the durable ledger can't carry: per-replica
	// admission (in-flight vs capacity), per-model in-flight + starvation (the
	// scale-from-zero demand signal), and the global request counters. Polled by
	// the page; this component is pure presentation.
	import type { RouterLiveMetrics } from '$lib/api/inference';
	import { compact } from './inference-telemetry';

	interface Props {
		live: RouterLiveMetrics | null;
	}
	let { live }: Props = $props();

	const pct = (a: number, b: number) => (b > 0 ? Math.min(100, Math.round((a / b) * 100)) : 0);
</script>

{#if !live || !live.available}
	<div
		class="rounded-xl border border-amber-200 bg-amber-50/60 px-4 py-3 text-sm text-amber-800 dark:border-amber-800/50 dark:bg-amber-950/30 dark:text-amber-200"
		data-testid="router-live-unavailable"
	>
		The inference router isn't reachable, so there are no live gauges right now. Start it with
		<code class="rounded bg-amber-100/70 px-1 py-px font-mono text-xs dark:bg-amber-900/40"
			>just dev up-router</code
		>. Historical charts below still render from the durable ledger.
	</div>
{:else}
	<div class="space-y-4" data-testid="router-live">
		<!-- Global counters since the router started. -->
		<div class="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-5">
			{#each [{ label: 'Requests', v: live.global.requests_total, tone: 'text-foreground' }, { label: 'Completed', v: live.global.completed_total, tone: 'text-emerald-600 dark:text-emerald-400' }, { label: 'Rejected 429', v: live.global.rejected_429_total, tone: 'text-amber-600 dark:text-amber-400' }, { label: 'Cancelled', v: live.global.cancelled_total, tone: 'text-muted-foreground' }, { label: 'Upstream errors', v: live.global.upstream_error_total, tone: 'text-rose-600 dark:text-rose-400' }] as c (c.label)}
				<div class="rounded-lg border border-border bg-card px-3 py-2">
					<div class="text-xs text-muted-foreground">{c.label}</div>
					<div class="text-lg font-semibold tabular-nums {c.tone}">{compact(c.v)}</div>
				</div>
			{/each}
		</div>

		<div class="grid gap-4 lg:grid-cols-2">
			<!-- Per-replica admission. -->
			<div class="rounded-xl border border-border bg-card p-4">
				<h3 class="mb-2 text-sm font-semibold text-foreground">Replicas</h3>
				{#if live.replicas.length === 0}
					<p class="text-sm text-muted-foreground">No upstream replicas advertised.</p>
				{:else}
					<ul class="space-y-2.5">
						{#each live.replicas as r (r.replica)}
							<li class="space-y-1">
								<div class="flex items-center justify-between gap-2 text-sm">
									<span class="min-w-0 truncate font-mono text-xs text-foreground/90">{r.replica}</span>
									<span class="flex shrink-0 items-center gap-1.5">
										{#if r.zone}<span class="text-xs text-muted-foreground">{r.zone}</span>{/if}
										<span
											class="inline-block size-1.5 rounded-full {r.live
												? 'bg-emerald-500'
												: 'bg-muted-foreground/40'}"
											title={r.live ? 'live' : 'not live'}
										></span>
										<span class="tabular-nums text-xs text-muted-foreground"
											>{r.in_flight}/{r.capacity}</span
										>
									</span>
								</div>
								<div class="h-1.5 overflow-hidden rounded-full bg-muted">
									<div
										class="h-full rounded-full {pct(r.in_flight, r.capacity) >= 100
											? 'bg-amber-500'
											: 'bg-sky-500'}"
										style="width: {pct(r.in_flight, r.capacity)}%"
									></div>
								</div>
							</li>
						{/each}
					</ul>
				{/if}
			</div>

			<!-- Per-model live demand. -->
			<div class="rounded-xl border border-border bg-card p-4">
				<h3 class="mb-2 text-sm font-semibold text-foreground">Models (live)</h3>
				{#if live.models.length === 0}
					<p class="text-sm text-muted-foreground">No model has served a request yet.</p>
				{:else}
					<div class="overflow-x-auto">
						<table class="w-full text-sm">
							<thead>
								<tr class="border-b border-border/60 text-left text-xs text-muted-foreground">
									<th class="py-1 pr-2 font-medium">Model</th>
									<th class="py-1 px-2 text-right font-medium">In-flight</th>
									<th class="py-1 px-2 text-right font-medium" title="Requests that found no live replica"
										>Starved</th
									>
									<th class="py-1 px-2 text-right font-medium">Avg ms</th>
									<th class="py-1 pl-2 text-right font-medium" title="Prompt + completion tokens"
										>Tokens</th
									>
								</tr>
							</thead>
							<tbody>
								{#each live.models as m (m.model)}
									<tr class="border-b border-border/30 last:border-0">
										<td class="min-w-0 max-w-[10rem] truncate py-1 pr-2 font-mono text-xs text-foreground/90"
											title={m.model}>{m.model}</td
										>
										<td class="py-1 px-2 text-right tabular-nums">{m.inflight}</td>
										<td
											class="py-1 px-2 text-right tabular-nums {m.starved > 0
												? 'text-amber-600 dark:text-amber-400'
												: 'text-muted-foreground'}">{m.starved}</td
										>
										<td class="py-1 px-2 text-right tabular-nums text-muted-foreground"
											>{m.avg_latency_ms != null ? Math.round(m.avg_latency_ms) : '—'}</td
										>
										<td class="py-1 pl-2 text-right tabular-nums text-muted-foreground"
											>{compact(m.prompt_tokens + m.completion_tokens)}</td
										>
									</tr>
								{/each}
							</tbody>
						</table>
					</div>
				{/if}
			</div>
		</div>
	</div>
{/if}
