<script lang="ts">
	// Capacity treemap — one breakdown level at a time. Drill is APP-controlled:
	// echarts' own zoom/breadcrumb is disabled (`nodeClick: false`,
	// `breadcrumb.show: false`); a click on a drillable node calls `onDrill`
	// with the full directory path and the parent refetches the next level
	// (backend lazy descent via `under=`). Mirrors the echarts lifecycle in
	// fleet/InferenceTimeseriesChart.svelte.
	import { onMount, onDestroy } from 'svelte';
	import * as echarts from 'echarts';
	import { formatBytes, formatCount } from './format';
	import type { TreemapNode } from './analytics-transforms';

	interface Props {
		nodes: TreemapNode[];
		onDrill: (path: string) => void;
		height?: number;
	}
	let { nodes, onDrill, height = 340 }: Props = $props();

	let el: HTMLDivElement | undefined = $state();
	let chart: echarts.ECharts | null = null;
	let ro: ResizeObserver | null = null;

	function render() {
		if (!chart) return;
		chart.setOption(
			{
				tooltip: {
					formatter: (info: { name?: string; value?: number; data?: TreemapNode }) => {
						const d = info.data;
						const head = `<b>${info.name ?? ''}</b>`;
						if (!d) return head;
						const drill = d.path && !d.isLeaf ? ' · click to drill' : '';
						return `${head}<br/>${formatBytes(d.value)} · ${formatCount(d.count)} files${drill}`;
					}
				},
				series: [
					{
						type: 'treemap',
						nodeClick: false,
						breadcrumb: { show: false },
						roam: false,
						left: 0,
						right: 0,
						top: 0,
						bottom: 0,
						label: {
							show: true,
							formatter: (p: { name: string; value: number | number[] }) =>
								`${p.name}\n${formatBytes(Array.isArray(p.value) ? p.value[0] : p.value)}`,
							color: '#fff',
							overflow: 'truncate'
						},
						upperLabel: { show: false },
						itemStyle: { borderColor: 'rgba(120,120,120,0.35)', gapWidth: 1 },
						emphasis: { label: { color: '#fff' } },
						data: nodes.map((n) => ({ ...n, value: Math.max(n.value, 0) }))
					}
				]
			},
			{ notMerge: true }
		);
	}

	onMount(() => {
		if (el) {
			chart = echarts.init(el);
			ro = new ResizeObserver(() => chart?.resize());
			ro.observe(el);
			chart.on('click', (params) => {
				const d = params.data as TreemapNode | undefined;
				if (d?.path && !d.isLeaf) onDrill(d.path);
			});
			render();
		}
	});

	onDestroy(() => {
		ro?.disconnect();
		chart?.dispose();
		chart = null;
	});

	// Re-render whenever the level data changes.
	$effect(() => {
		void nodes;
		render();
	});
</script>

<div bind:this={el} style="height: {height}px; width: 100%"></div>
