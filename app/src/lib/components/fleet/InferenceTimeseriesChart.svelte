<script lang="ts">
	// A thin echarts wrapper for the per-model inference timeseries. Pure
	// presentation: the page pivots the ledger rollups (inference-telemetry.ts)
	// into aligned per-model series and hands them here. One line per model over a
	// shared bucket axis. Mirrors the echarts lifecycle in MetricsPanel.svelte.
	import { onMount, onDestroy } from 'svelte';
	import * as echarts from 'echarts';
	import { bucketLabel } from './inference-telemetry';

	interface Props {
		categories: string[];
		series: { name: string; data: (number | null)[] }[];
		unit: string;
		/** When true, draw across null gaps (counts); false shows gaps (latency/rate). */
		connectNulls?: boolean;
		height?: number;
	}
	let { categories, series, unit, connectNulls = false, height = 240 }: Props = $props();

	let el: HTMLDivElement | undefined = $state();
	let chart: echarts.ECharts | null = null;
	let ro: ResizeObserver | null = null;

	// Mid-grey axis/legend reads acceptably in both light and dark themes (echarts
	// can't see CSS vars); the series colours come from the default palette.
	const AXIS = '#9ca3af';

	function render() {
		if (!chart) return;
		chart.setOption(
			{
				grid: { left: 52, right: 16, top: 30, bottom: 26 },
				tooltip: { trigger: 'axis' },
				legend: { type: 'scroll', top: 0, textStyle: { color: AXIS } },
				xAxis: {
					type: 'category',
					data: categories,
					boundaryGap: false,
					axisLabel: { formatter: (v: string) => bucketLabel(v), color: AXIS },
					axisLine: { lineStyle: { color: 'rgba(120,120,120,0.35)' } }
				},
				yAxis: {
					type: 'value',
					name: unit,
					nameTextStyle: { color: AXIS, align: 'left' },
					axisLabel: { color: AXIS },
					splitLine: { lineStyle: { color: 'rgba(120,120,120,0.15)' } }
				},
				series: series.map((s) => ({
					name: s.name,
					type: 'line',
					smooth: true,
					showSymbol: false,
					connectNulls,
					data: s.data
				}))
			},
			{ notMerge: true }
		);
	}

	onMount(() => {
		if (el) {
			chart = echarts.init(el);
			ro = new ResizeObserver(() => chart?.resize());
			ro.observe(el);
			render();
		}
	});

	onDestroy(() => {
		ro?.disconnect();
		chart?.dispose();
		chart = null;
	});

	// Re-render whenever the inputs change.
	$effect(() => {
		void categories;
		void series;
		void unit;
		void connectNulls;
		render();
	});
</script>

<div bind:this={el} style="height: {height}px; width: 100%"></div>
