<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import { catalogueDownloadUrl } from '$lib/api/client';
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

	let model = $state<GpModel | null>(null);
	let fetching = $state(false);
	let error = $state<string | null>(null);
	// eslint-disable-next-line @typescript-eslint/no-explicit-any
	let Plotly: any = null;

	onMount(async () => {
		// plotly.js-dist-min ships no .d.ts; opt-in to any via @ts-expect-error.
		// @ts-expect-error no types for plotly.js-dist-min
		const mod = await import('plotly.js-dist-min');
		Plotly = mod.default ?? mod;
	});

	onDestroy(() => {
		for (const el of [meanEl, stdEl, eiEl]) {
			if (el && Plotly) Plotly.purge(el);
		}
	});

	// Fetch on entry change. Keep the previous model visible while the next
	// one is in flight — swapping `model` atomically on success lets Plotly.react
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
		fetch(catalogueDownloadUrl(entry.storage_path), { signal: controller.signal })
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

	function layout(title: string) {
		return {
			title: { text: title, font: { size: 13 } },
			margin: { l: 50, r: 20, t: 36, b: 40 },
			xaxis: { title: 'A', range: [0, 1] },
			yaxis: { title: 'D', range: [0, 1] },
			paper_bgcolor: 'rgba(0,0,0,0)',
			plot_bgcolor: 'rgba(0,0,0,0)',
			font: { color: '#888', size: 11 }
		};
	}

	function observedOrNextMarker(model: GpModel) {
		if (!model.next_candidate) return [];
		return [
			{
				x: [model.next_candidate.a],
				y: [model.next_candidate.d],
				mode: 'markers',
				marker: {
					size: 14,
					color: '#fbbf24',
					symbol: 'x',
					line: { color: '#000', width: 2 }
				},
				name: 'Next',
				showlegend: false,
				hovertemplate: 'Next candidate<br>A=%{x:.3f}<br>D=%{y:.3f}<extra></extra>'
			}
		];
	}

	function render(m: GpModel) {
		if (!Plotly) return;
		const config = { responsive: true, displaylogo: false };

		if (meanEl) {
			Plotly.react(
				meanEl,
				[
					{
						z: m.gp_mean,
						x: m.A_lin,
						y: m.D_lin,
						type: 'heatmap',
						colorscale: 'Viridis',
						colorbar: { title: 'μ', len: 0.9 },
						hovertemplate: 'A=%{x:.3f}<br>D=%{y:.3f}<br>μ=%{z:.3f}<extra></extra>'
					},
					...observedOrNextMarker(m)
				],
				layout('Posterior mean μ'),
				config
			);
		}

		if (stdEl) {
			Plotly.react(
				stdEl,
				[
					{
						z: m.gp_std,
						x: m.A_lin,
						y: m.D_lin,
						type: 'heatmap',
						colorscale: 'Inferno',
						colorbar: { title: 'σ', len: 0.9 },
						hovertemplate: 'A=%{x:.3f}<br>D=%{y:.3f}<br>σ=%{z:.3f}<extra></extra>'
					},
					...observedOrNextMarker(m)
				],
				layout('Uncertainty σ'),
				config
			);
		}

		if (eiEl) {
			Plotly.react(
				eiEl,
				[
					{
						z: m.ei,
						x: m.A_lin,
						y: m.D_lin,
						type: 'heatmap',
						colorscale: 'Hot',
						reversescale: true,
						colorbar: { title: 'EI', len: 0.9 },
						hovertemplate: 'A=%{x:.3f}<br>D=%{y:.3f}<br>EI=%{z:.4f}<extra></extra>'
					},
					...observedOrNextMarker(m)
				],
				layout('Expected improvement'),
				config
			);
		}
	}

	$effect(() => {
		if (model && Plotly) render(model);
	});
</script>

<div class="flex flex-col gap-3">
	<!-- Chart divs always mounted so Plotly.react can transition smoothly
	     between iterations instead of re-initializing on every scrub. -->
	<div class="relative grid grid-cols-1 gap-3 lg:grid-cols-3">
		<div class="rounded-lg border border-border bg-card p-2">
			<div bind:this={meanEl} class="h-80 w-full"></div>
		</div>
		<div class="rounded-lg border border-border bg-card p-2">
			<div bind:this={stdEl} class="h-80 w-full"></div>
		</div>
		<div class="rounded-lg border border-border bg-card p-2">
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
