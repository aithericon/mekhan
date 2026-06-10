<script lang="ts">
	// Growth trend — one line per file server over the snapshot buckets that
	// pivotSnapshots (analytics-transforms.ts) aligned. Missing buckets are real
	// gaps (no snapshot captured), so nulls are NOT connected. Mirrors the
	// echarts lifecycle in fleet/InferenceTimeseriesChart.svelte.
	import { onMount, onDestroy } from 'svelte';
	import * as echarts from 'echarts';
	import { formatBytes, formatCount } from './format';

	interface Props {
		categories: string[];
		series: { name: string; data: (number | null)[] }[];
		mode?: 'bytes' | 'count';
		height?: number;
	}
	let { categories, series, mode = 'bytes', height = 260 }: Props = $props();

	let el: HTMLDivElement | undefined = $state();
	let chart: echarts.ECharts | null = null;
	let ro: ResizeObserver | null = null;

	// Mid-grey axis/legend reads acceptably in both light and dark themes
	// (echarts can't see CSS vars); the series colours come from the palette.
	const AXIS = '#9ca3af';

	const fmt = (v: number) => (mode === 'bytes' ? formatBytes(v) : formatCount(v));

	// Growth windows span days, so keep the date in the axis label: `M/D HH:MM`.
	function bucketLabel(iso: string): string {
		const d = new Date(iso);
		if (Number.isNaN(d.getTime())) return iso;
		const hm = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
		return `${d.getMonth() + 1}/${d.getDate()} ${hm}`;
	}

	function render() {
		if (!chart) return;
		chart.setOption(
			{
				grid: { left: 64, right: 16, top: 30, bottom: 26 },
				tooltip: { trigger: 'axis', valueFormatter: (v: number | null) => (v == null ? '—' : fmt(v)) },
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
					axisLabel: { color: AXIS, formatter: (v: number) => fmt(v) },
					splitLine: { lineStyle: { color: 'rgba(120,120,120,0.15)' } }
				},
				series: series.map((s) => ({
					name: s.name,
					type: 'line',
					smooth: true,
					showSymbol: false,
					connectNulls: false,
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
		void mode;
		render();
	});
</script>

<div bind:this={el} style="height: {height}px; width: 100%"></div>
