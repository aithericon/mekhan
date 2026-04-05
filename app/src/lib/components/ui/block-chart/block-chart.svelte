<script lang="ts">
	import type { BlockChartProps } from './types.js';
	import * as Chart from '$lib/components/ui/chart/index.js';
	import { AreaChart, BarChart, LineChart, PieChart } from 'layerchart';
	import { scaleBand } from 'd3-scale';

	let { chart_type, data, x, series, caption, height, x_label, y_label }: BlockChartProps =
		$props();

	const DEFAULT_COLORS = [
		'var(--color-chart-1)',
		'var(--color-chart-2)',
		'var(--color-chart-3)',
		'var(--color-chart-4)',
		'var(--color-chart-5)'
	];

	const resolvedSeries = $derived(
		series?.map((s, i) => ({
			...s,
			color: s.color ?? DEFAULT_COLORS[i % DEFAULT_COLORS.length]
		})) ?? []
	);

	const chartConfig = $derived(
		Object.fromEntries(
			resolvedSeries.map((s) => [s.key, { label: s.label ?? s.key, color: s.color }])
		) satisfies Chart.ChartConfig
	);

	const pieConfig = $derived.by(() => {
		if (chart_type !== 'pie') return {} satisfies Chart.ChartConfig;
		const xKey = x ?? 'x';
		return Object.fromEntries(
			data.map((d, i) => {
				const key = String(d[xKey] ?? d['category'] ?? d['label'] ?? d['name'] ?? i);
				return [key, { label: key, color: DEFAULT_COLORS[i % DEFAULT_COLORS.length] }];
			})
		) satisfies Chart.ChartConfig;
	});

	const axisProps = $derived({
		xAxis: x_label ? { label: x_label } : undefined,
		yAxis: y_label ? { label: y_label } : undefined
	});
</script>

<figure
	class="overflow-hidden rounded-xl border border-border bg-card/70 shadow-sm"
	data-testid="block-chart"
>
	{#if caption}
		<figcaption class="pt-3 pb-2 text-center text-base text-muted-foreground">
			{caption}
		</figcaption>
	{/if}
	<div class="p-4" style="height: {height ?? '300px'}">
		{#if chart_type === 'pie'}
			{@const valueKey = resolvedSeries[0]?.key ?? 'value'}
			{@const labelKey = x ?? 'category'}
			<Chart.Container config={pieConfig} class="size-full">
				<PieChart
					{data}
					key={labelKey}
					value={valueKey}
					c={labelKey}
					cRange={data.map((_d, i) => DEFAULT_COLORS[i % DEFAULT_COLORS.length])}
				>
					{#snippet tooltip()}
						<Chart.Tooltip />
					{/snippet}
				</PieChart>
			</Chart.Container>
		{:else}
			<Chart.Container config={chartConfig} class="size-full">
				{#if chart_type === 'area'}
					<AreaChart
						{data}
						x={x ?? 'x'}
						xScale={scaleBand()}
						axis={true}
						series={resolvedSeries}
						props={axisProps}
					>
						{#snippet tooltip()}
							<Chart.Tooltip />
						{/snippet}
					</AreaChart>
				{:else if chart_type === 'bar'}
					<BarChart
						{data}
						x={x ?? 'x'}
						xScale={scaleBand().padding(0.25)}
						axis={true}
						seriesLayout="group"
						series={resolvedSeries}
						props={axisProps}
					>
						{#snippet tooltip()}
							<Chart.Tooltip />
						{/snippet}
					</BarChart>
				{:else if chart_type === 'line'}
					<LineChart
						{data}
						x={x ?? 'x'}
						xScale={scaleBand()}
						axis={true}
						series={resolvedSeries}
						props={axisProps}
					>
						{#snippet tooltip()}
							<Chart.Tooltip />
						{/snippet}
					</LineChart>
				{/if}
			</Chart.Container>
		{/if}
	</div>
</figure>
