<script lang="ts">
	// One breakdown dimension as a horizontal bar list or a donut. Pure
	// presentation — the parent fetches/sorts/labels the buckets
	// (analytics-transforms.ts). Mirrors the echarts lifecycle in
	// fleet/InferenceTimeseriesChart.svelte.
	import { onMount, onDestroy } from 'svelte';
	import * as echarts from 'echarts';
	import { formatBytes, formatCount } from './format';
	import type { BreakdownBucket } from '$lib/api/analytics';

	interface Props {
		buckets: BreakdownBucket[];
		kind?: 'bar' | 'donut';
		mode?: 'bytes' | 'count';
		/** Optional key prettifier (sizeClassLabel / ageCohortLabel). */
		labelOf?: (key: string) => string;
		height?: number;
	}
	let { buckets, kind = 'bar', mode = 'bytes', labelOf = (k) => k, height = 280 }: Props = $props();

	let el: HTMLDivElement | undefined = $state();
	let chart: echarts.ECharts | null = null;
	let ro: ResizeObserver | null = null;

	// Mid-grey axis/legend reads acceptably in both light and dark themes
	// (echarts can't see CSS vars).
	const AXIS = '#9ca3af';

	const valueOf = (b: BreakdownBucket) => (mode === 'bytes' ? b.bytes : b.count);
	const fmt = (v: number) => (mode === 'bytes' ? formatBytes(v) : formatCount(v));

	function render() {
		if (!chart) return;
		const labels = buckets.map((b) => labelOf(b.key));
		const values = buckets.map(valueOf);
		if (kind === 'donut') {
			chart.setOption(
				{
					tooltip: {
						trigger: 'item',
						formatter: (info: { name?: string; value?: number; percent?: number }) =>
							`${info.name}: ${fmt(info.value ?? 0)} (${info.percent ?? 0}%)`
					},
					legend: { type: 'scroll', bottom: 0, textStyle: { color: AXIS } },
					series: [
						{
							type: 'pie',
							radius: ['45%', '70%'],
							center: ['50%', '45%'],
							itemStyle: { borderColor: 'rgba(120,120,120,0.35)', borderWidth: 1 },
							label: { show: false },
							data: buckets.map((b) => ({ name: labelOf(b.key), value: valueOf(b) }))
						}
					]
				},
				{ notMerge: true }
			);
		} else {
			chart.setOption(
				{
					grid: { left: 8, right: 24, top: 8, bottom: 26, containLabel: true },
					tooltip: {
						trigger: 'axis',
						axisPointer: { type: 'shadow' },
						valueFormatter: (v: number) => fmt(v)
					},
					xAxis: {
						type: 'value',
						axisLabel: { color: AXIS, formatter: (v: number) => fmt(v) },
						splitLine: { lineStyle: { color: 'rgba(120,120,120,0.15)' } }
					},
					yAxis: {
						type: 'category',
						// Largest at the top.
						inverse: true,
						data: labels,
						axisLabel: { color: AXIS },
						axisLine: { lineStyle: { color: 'rgba(120,120,120,0.35)' } }
					},
					series: [{ type: 'bar', barMaxWidth: 18, data: values }]
				},
				{ notMerge: true }
			);
		}
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
		void buckets;
		void kind;
		void mode;
		void labelOf;
		render();
	});
</script>

<div bind:this={el} style="height: {height}px; width: 100%"></div>
