<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import * as echarts from 'echarts';
	import { catalogueDownloadUrl } from '$lib/api/client';
	import { authFetch } from '$lib/auth/fetch';
	import type { LiveArtifactEntry } from '$lib/api/client';

	/**
	 * Renders a GP posterior artifact (`gp_model.json`) as three heatmaps:
	 *   μ (posterior mean) · σ (uncertainty) · EI (expected improvement),
	 * with a cross marking the next proposed candidate.
	 *
	 * Expected JSON shape (from `GaussianProcesses/python/fit_gp.py`):
	 *   {
	 *     gp_mean: [[..]],  gp_std: [[..]],  ei: [[..]],
	 *     A_lin: [..],      D_lin: [..],
	 *     next_candidate: { a: number, d: number },
	 *     n_observations?: number,
	 *     kernel_params?: Record<string, unknown>
	 *   }
	 */

	interface Props {
		entry: LiveArtifactEntry;
	}
	let { entry }: Props = $props();

	interface GpModel {
		gp_mean: number[][];
		gp_std: number[][];
		ei: number[][];
		A_lin: number[];
		D_lin: number[];
		next_candidate?: { a: number; d: number };
		n_observations?: number;
		kernel_params?: Record<string, unknown>;
		f_best_used?: number;
	}

	let meanEl: HTMLDivElement | undefined = $state();
	let stdEl: HTMLDivElement | undefined = $state();
	let eiEl: HTMLDivElement | undefined = $state();

	let meanChart: echarts.ECharts | null = $state(null);
	let stdChart: echarts.ECharts | null = $state(null);
	let eiChart: echarts.ECharts | null = $state(null);

	let model = $state<GpModel | null>(null);
	let fetching = $state(false);
	let error = $state<string | null>(null);

	// Unified Viridis palette across all three panels so spatial correlations
	// are legible by eye: each panel keeps its own colorbar range so the
	// absolute values stay unambiguous, but the shared perceptual scale lets
	// you directly read off "high μ here coincides with low σ there".
	// Viridis is also perceptually uniform, colorblind-safe, and grayscale-safe.
	const VIRIDIS = ['#440154', '#3b528b', '#21918c', '#5ec962', '#fde725'];

	onMount(() => {
		if (meanEl) meanChart = echarts.init(meanEl);
		if (stdEl) stdChart = echarts.init(stdEl);
		if (eiEl) eiChart = echarts.init(eiEl);
		const onResize = () => {
			meanChart?.resize();
			stdChart?.resize();
			eiChart?.resize();
		};
		window.addEventListener('resize', onResize);
		return () => window.removeEventListener('resize', onResize);
	});

	onDestroy(() => {
		meanChart?.dispose();
		stdChart?.dispose();
		eiChart?.dispose();
		meanChart = null;
		stdChart = null;
		eiChart = null;
	});

	// Fetch on entry change. Keep the previous model visible while the next
	// one is in flight — swapping `model` atomically on success lets ECharts
	// transition the existing canvas instead of unmounting + re-initing.
	$effect(() => {
		const id = entry.artifact_id ?? entry.id;
		void id;
		error = null;
		if (!entry.storage_path) {
			error = 'no storage_path';
			return;
		}
		fetching = true;
		const controller = new AbortController();
		authFetch(catalogueDownloadUrl(entry.storage_path), { signal: controller.signal })
			.then((r) => {
				if (!r.ok) throw new Error(`fetch failed: ${r.status}`);
				return r.json();
			})
			.then((j: GpModel) => {
				model = j;
				fetching = false;
			})
			.catch((e) => {
				if (controller.signal.aborted) return;
				error = e instanceof Error ? e.message : String(e);
				fetching = false;
			});
		return () => controller.abort();
	});

	/**
	 * Transform a 2D grid `Z[y][x]` into ECharts heatmap tuples `[xIdx, yIdx, v]`.
	 * Also returns min/max so visualMap can be scaled to the data range rather
	 * than the global default.
	 */
	function grid(z: number[][]): { data: [number, number, number][]; min: number; max: number } {
		const out: [number, number, number][] = [];
		let min = Infinity;
		let max = -Infinity;
		for (let yi = 0; yi < z.length; yi++) {
			const row = z[yi];
			for (let xi = 0; xi < row.length; xi++) {
				const v = row[xi];
				out.push([xi, yi, v]);
				if (v < min) min = v;
				if (v > max) max = v;
			}
		}
		if (!Number.isFinite(min)) min = 0;
		if (!Number.isFinite(max)) max = 1;
		return { data: out, min, max };
	}

	/** Locate the nearest grid index for a [0,1] coordinate. */
	function nearestIdx(v: number, axis: number[]): number {
		if (axis.length === 0) return 0;
		let best = 0;
		let bestDist = Math.abs(v - axis[0]);
		for (let i = 1; i < axis.length; i++) {
			const d = Math.abs(v - axis[i]);
			if (d < bestDist) {
				bestDist = d;
				best = i;
			}
		}
		return best;
	}

	function fmt(x: number): string {
		return x.toFixed(3);
	}

	function buildOption(
		m: GpModel,
		which: 'mean' | 'std' | 'ei',
		title: string,
		colors: string[]
	): echarts.EChartsOption {
		const z = which === 'mean' ? m.gp_mean : which === 'std' ? m.gp_std : m.ei;
		const { data, min, max } = grid(z);

		// Category axes labelled with the underlying A/D values but thinned to
		// avoid overlap. ECharts picks label interval automatically when 'auto'.
		const xLabels = m.A_lin.map(fmt);
		const yLabels = m.D_lin.map(fmt);

		const markers: echarts.SeriesOption[] = [];
		if (m.next_candidate) {
			const xi = nearestIdx(m.next_candidate.a, m.A_lin);
			const yi = nearestIdx(m.next_candidate.d, m.D_lin);
			markers.push({
				name: 'Next',
				type: 'scatter',
				symbol: 'path://M0,-10L0,10M-10,0L10,0', // clean crosshair
				symbolSize: 18,
				itemStyle: { color: '#fbbf24', borderColor: '#000', borderWidth: 2 },
				data: [
					{
						value: [xi, yi],
						// Keep the tooltip meaningful even though the marker is 1 point.
						name: 'next candidate'
					}
				],
				tooltip: {
					formatter: () =>
						`next candidate<br/>A=${fmt(m.next_candidate!.a)}<br/>D=${fmt(m.next_candidate!.d)}`
				},
				z: 10
			});
		}

		const valueUnit = which === 'mean' ? 'μ' : which === 'std' ? 'σ' : 'EI';

		return {
			animation: false,
			title: {
				text: title,
				left: 'center',
				top: 2,
				textStyle: { color: '#888', fontSize: 13, fontWeight: 'normal' }
			},
			grid: { left: 56, right: 96, top: 30, bottom: 48 },
			tooltip: {
				trigger: 'item',
				confine: true,
				formatter: (p) => {
					const param = p as { seriesName?: string; value?: [number, number, number] };
					if (param.seriesName === 'Next') return 'next candidate';
					const [xi, yi, v] = param.value ?? [0, 0, 0];
					return `A=${xLabels[xi]}<br/>D=${yLabels[yi]}<br/>${valueUnit}=${v.toPrecision(4)}`;
				}
			},
			xAxis: {
				type: 'category',
				data: xLabels,
				name: 'A',
				nameLocation: 'middle',
				nameGap: 28,
				nameTextStyle: { color: '#888', fontSize: 11 },
				axisLine: { lineStyle: { color: '#555' } },
				axisTick: { show: false },
				axisLabel: {
					color: '#888',
					fontSize: 10,
					interval: Math.max(1, Math.floor(xLabels.length / 6)) - 1
				}
			},
			yAxis: {
				type: 'category',
				data: yLabels,
				name: 'D',
				nameLocation: 'middle',
				nameGap: 40,
				nameRotate: 90,
				nameTextStyle: { color: '#888', fontSize: 11 },
				axisLine: { lineStyle: { color: '#555' } },
				axisTick: { show: false },
				axisLabel: {
					color: '#888',
					fontSize: 10,
					interval: Math.max(1, Math.floor(yLabels.length / 6)) - 1
				}
			},
			visualMap: {
				min,
				max,
				calculable: true,
				orient: 'vertical',
				right: 18,
				top: 'middle',
				itemHeight: 160,
				itemWidth: 10,
				textStyle: { color: '#888', fontSize: 10 },
				formatter: (v) => (typeof v === 'number' ? v.toExponential(1) : String(v)),
				inRange: { color: colors }
			},
			series: [
				{
					name: valueUnit,
					type: 'heatmap',
					data,
					progressive: 4000,
					animation: false,
					emphasis: { itemStyle: { borderColor: '#fff', borderWidth: 0.5 } }
				},
				...markers
			]
		};
	}

	function render(m: GpModel) {
		if (meanChart) {
			meanChart.setOption(buildOption(m, 'mean', 'Posterior mean μ', VIRIDIS), {
				notMerge: true,
				lazyUpdate: true
			});
		}
		if (stdChart) {
			stdChart.setOption(buildOption(m, 'std', 'Uncertainty σ', VIRIDIS), {
				notMerge: true,
				lazyUpdate: true
			});
		}
		if (eiChart) {
			eiChart.setOption(buildOption(m, 'ei', 'Expected improvement', VIRIDIS), {
				notMerge: true,
				lazyUpdate: true
			});
		}
	}

	$effect(() => {
		const m = model;
		// Also depend on chart instances so re-runs fire once charts initialize.
		void meanChart;
		void stdChart;
		void eiChart;
		if (m) render(m);
	});
</script>

<div class="flex flex-col gap-3">
	<!-- Chart divs always mounted so setOption transitions smoothly between
	     iterations instead of re-initializing on every scrub. -->
	<div class="relative grid grid-cols-1 gap-3 lg:grid-cols-3">
		<div class="rounded-lg bg-white p-2 shadow-md dark:bg-zinc-900">
			<div bind:this={meanEl} class="h-80 w-full"></div>
		</div>
		<div class="rounded-lg bg-white p-2 shadow-md dark:bg-zinc-900">
			<div bind:this={stdEl} class="h-80 w-full"></div>
		</div>
		<div class="rounded-lg bg-white p-2 shadow-md dark:bg-zinc-900">
			<div bind:this={eiEl} class="h-80 w-full"></div>
		</div>
		{#if fetching && !model}
			<div
				class="pointer-events-none absolute inset-0 flex items-center justify-center text-xs text-muted-foreground"
			>
				Loading posterior…
			</div>
		{/if}
		{#if fetching && model}
			<div
				class="pointer-events-none absolute right-3 top-3 rounded-full bg-primary/80 px-2 py-0.5 text-[10px] font-medium text-primary-foreground"
			>
				updating…
			</div>
		{/if}
	</div>

	<!-- Reserve a fixed row so the summary line never shifts the chart height -->
	<div class="flex min-h-5 flex-wrap items-center gap-4 text-xs text-muted-foreground">
		{#if error}
			<span class="text-red-500">{error}</span>
		{:else if model}
			{#if model.n_observations !== undefined}
				<span>observations: <b class="tabular-nums">{model.n_observations}</b></span>
			{/if}
			{#if model.next_candidate}
				<span>
					next: A=<b class="tabular-nums">{model.next_candidate.a.toFixed(3)}</b>, D=<b
						class="tabular-nums">{model.next_candidate.d.toFixed(3)}</b
					>
				</span>
			{/if}
			{#if model.f_best_used !== undefined}
				<span>f_best: <b class="tabular-nums">{model.f_best_used.toPrecision(4)}</b></span>
			{/if}
		{/if}
	</div>
</div>
