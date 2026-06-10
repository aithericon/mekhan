<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import * as echarts from 'echarts';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { getProcessMetricsSummary } from '$lib/api/client';
	import type { createProcessLiveStore } from '$lib/stores/process-live.svelte';

	type Store = ReturnType<typeof createProcessLiveStore>;
	interface Props {
		processId: string;
		store: Store;
		/** Rail/sidebar mode: no controls, no key picker, tight short chart.
		 *  The full panel (with window/key/signal controls) lives on the
		 *  Metrics tab; compact embeds elsewhere share the same store. */
		compact?: boolean;
	}
	let { processId, store, compact = false }: Props = $props();

	let chartEl: HTMLDivElement | undefined = $state();
	let chart: echarts.ECharts | null = $state(null);

	let availableKeys = $state<string[]>([]);
	let selectedKeys = $state<string[]>([]);
	let signalKeyInput = $state<string>('');
	let windowChoice = $state<number>(3600);

	const WINDOW_CHOICES: { label: string; seconds: number }[] = [
		{ label: '1m', seconds: 60 },
		{ label: '5m', seconds: 300 },
		{ label: '15m', seconds: 900 },
		{ label: '1h', seconds: 3600 },
		{ label: '6h', seconds: 21600 },
		{ label: '24h', seconds: 86400 }
	];

	onMount(async () => {
		// Discover which metric keys this process has emitted.
		try {
			const summary = await getProcessMetricsSummary(processId);
			availableKeys = summary.map((s) => s.key);
			// Default: pick the first up to 4 so chart isn't empty.
			if (selectedKeys.length === 0 && availableKeys.length > 0) {
				selectedKeys = availableKeys.slice(0, Math.min(4, availableKeys.length));
				store.setKeys(selectedKeys);
			}
		} catch {
			availableKeys = [];
		}

		if (chartEl) {
			chart = echarts.init(chartEl);
			const ro = new ResizeObserver(() => chart?.resize());
			ro.observe(chartEl);
		}
	});

	onDestroy(() => {
		chart?.dispose();
		chart = null;
	});

	// Push data into the chart whenever the store's metrics change.
	// Read store.metrics BEFORE the chart-null check so reactivity tracks it
	// even when chart hasn't initialized yet (first-run early return otherwise
	// would skip dep registration and never re-fire).
	$effect(() => {
		const metrics = store.metrics;
		if (!chart) return;
		const keys = Object.keys(metrics.series);
		const option: echarts.EChartsOption = {
			animation: false,
			grid: compact
				? { left: 40, right: 8, top: 24, bottom: 20 }
				: { left: 56, right: 40, top: 56, bottom: 56 },
			tooltip: { trigger: 'axis', axisPointer: { type: 'line' }, confine: true },
			...(compact
				? {}
				: {
						toolbox: {
							right: 8,
							top: 4,
							feature: {
								dataZoom: { yAxisIndex: 'none', icon: { back: 'path://' } },
								restore: {},
								saveAsImage: {}
							},
							iconStyle: { borderColor: '#888' }
						}
					}),
			legend: {
				data: keys,
				type: 'scroll',
				top: compact ? 0 : 4,
				left: compact ? 0 : 'center',
				right: compact ? 0 : 140,
				textStyle: { color: '#888', fontSize: compact ? 10 : 11 },
				pageIconColor: '#888',
				pageIconInactiveColor: '#444',
				pageTextStyle: { color: '#888' },
				itemWidth: compact ? 10 : 14,
				itemHeight: compact ? 8 : 10,
				itemGap: compact ? 8 : 10
			},
			xAxis: {
				type: 'time',
				axisLine: { lineStyle: { color: '#555' } },
				axisLabel: { color: '#888', ...(compact ? { fontSize: 10, hideOverlap: true } : {}) }
			},
			yAxis: {
				type: 'value',
				scale: true,
				axisLine: { lineStyle: { color: '#555' } },
				axisLabel: { color: '#888', ...(compact ? { fontSize: 10 } : {}) },
				splitLine: { lineStyle: { color: 'rgba(127,127,127,0.15)' } }
			},
			dataZoom: [
				// Wheel + pinch zoom on the plot area itself; no space cost.
				{ type: 'inside', xAxisIndex: 0, filterMode: 'none' },
				// Thin slider at the bottom for precise range selection
				// (full panel only — compact keeps the footprint minimal).
				...(compact
					? []
					: [
							{
								type: 'slider' as const,
								xAxisIndex: 0,
								height: 18,
								bottom: 8,
								borderColor: 'transparent',
								backgroundColor: 'rgba(127,127,127,0.08)',
								fillerColor: 'rgba(127,127,127,0.25)',
								handleSize: '100%',
								textStyle: { color: '#888', fontSize: 10 }
							}
						])
			],
			series: keys.map((k) => ({
				name: k,
				type: 'line',
				// A sparse series (a run that emits a handful of gauge points —
				// or just one) is INVISIBLE with symbols off: a 1-point line
				// draws nothing. Dot the points when there are few; dense
				// streams stay clean lines.
				showSymbol: metrics.series[k].length <= 24,
				symbolSize: 5,
				smooth: false,
				sampling: 'lttb',
				data: metrics.series[k].map((p) => [p.t, p.v])
			}))
		};
		chart.setOption(option, { notMerge: true, lazyUpdate: true });
	});

	function toggleKey(k: string) {
		if (selectedKeys.includes(k)) {
			selectedKeys = selectedKeys.filter((x) => x !== k);
		} else {
			selectedKeys = [...selectedKeys, k];
		}
		store.setKeys(selectedKeys);
	}

	function applySignalKey() {
		store.setSignalKey(signalKeyInput.trim() || undefined);
	}

	function setWindow(sec: number) {
		windowChoice = sec;
		store.setWindowSeconds(sec);
	}

	const statusLabel = $derived(
		store.metricStatus === 'streaming'
			? 'live'
			: store.metricStatus === 'reconnecting'
				? 'reconnecting…'
				: store.metricStatus === 'loading'
					? 'loading…'
					: store.metricStatus
	);
	const statusDotClass = $derived(
		store.metricStatus === 'streaming'
			? 'bg-green-500'
			: store.metricStatus === 'error'
				? 'bg-red-500'
				: 'bg-amber-500'
	);
</script>

<div class="flex flex-col {compact ? 'gap-2' : 'gap-3'}">
	{#if !compact}
		<!-- Controls row -->
		<div class="flex flex-wrap items-center gap-3">
			<div class="flex items-center gap-1 text-sm">
				<span class="inline-block size-2 rounded-full {statusDotClass}"></span>
				<span class="text-muted-foreground">{statusLabel}</span>
			</div>

			<div class="flex items-center gap-1">
				{#each WINDOW_CHOICES as w}
					<Button
						size="sm"
						variant={windowChoice === w.seconds ? 'default' : 'ghost'}
						onclick={() => setWindow(w.seconds)}
					>
						{w.label}
					</Button>
				{/each}
			</div>

			<div class="flex items-center gap-2">
				<Input
					placeholder="signal_key (optional)"
					class="h-8 w-52 text-sm"
					bind:value={signalKeyInput}
					onkeydown={(e: KeyboardEvent) => e.key === 'Enter' && applySignalKey()}
				/>
				<Button size="sm" variant="outline" onclick={applySignalKey}>Apply</Button>
			</div>
		</div>

		<!-- Key picker -->
		{#if availableKeys.length > 0}
			<div class="flex flex-wrap gap-1">
				{#each availableKeys as k}
					<Button
						size="sm"
						variant={selectedKeys.includes(k) ? 'default' : 'outline'}
						class="h-7 font-mono text-sm"
						onclick={() => toggleKey(k)}
					>
						{k}
					</Button>
				{/each}
			</div>
		{/if}
	{/if}

	<!-- Chart. Compact mode is embedded inside an existing card, so it skips
	     the panel's own card chrome and runs a short chart. -->
	<div class={compact ? '' : 'rounded-lg border border-border bg-card p-2'}>
		<div bind:this={chartEl} class="{compact ? 'h-44' : 'h-80'} w-full"></div>
		{#if Object.keys(store.metrics.series).length === 0 && store.metricStatus !== 'loading'}
			<div class="flex items-center justify-center py-6 text-sm text-muted-foreground">
				{compact
					? 'No metric points in window.'
					: 'No metric points in window. Pick a key or widen the time range.'}
			</div>
		{/if}
	</div>

	{#if !compact && store.metrics.bucketSeconds > 0}
		<p class="text-sm text-muted-foreground">
			Downsampled to {store.metrics.bucketSeconds}s buckets for backfill window.
		</p>
	{/if}

	{#if store.error}
		<p class="text-sm text-red-500">{store.error}</p>
	{/if}
</div>
