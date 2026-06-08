<script lang="ts">
	import { page } from '$app/stores';
	import {
		getCatalogueLineage,
		catalogueDownloadUrl,
		type LineageResponse
	} from '$lib/api/client';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Separator } from '$lib/components/ui/separator';
	import { ArtifactCard } from '$lib/components/catalogue';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import FileBox from '@lucide/svelte/icons/file-box';
	import GitBranch from '@lucide/svelte/icons/git-branch';
	import Activity from '@lucide/svelte/icons/activity';

	import { browser } from '$app/environment';
	import { tick } from 'svelte';

	// ── State ──────────────────────────────────────────────────────────────────
	let lineage = $state<LineageResponse | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let expandedId = $state<string | null>(null);
	const highlightArtifact = browser
		? new URLSearchParams(window.location.search).get('artifact')
		: null;

	// Producing instance: any artifact's source_net is the net_id
	// (`mekhan-{instance_uuid}`). /instances/{id} → process view.
	const instanceId = $derived.by(() => {
		const net = lineage?.steps.flatMap((s) => s.artifacts).find((a) => a.source_net)?.source_net;
		return net ? net.replace(/^mekhan-/, '') : null;
	});

	// ── Category colours ───────────────────────────────────────────────────────
	const categoryColors: Record<string, string> = {
		model: 'bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200',
		dataset: 'bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200',
		plot: 'bg-purple-100 text-purple-800 dark:bg-purple-900 dark:text-purple-200',
		log: 'bg-gray-100 text-gray-700 dark:bg-gray-800 dark:text-gray-300',
		checkpoint: 'bg-orange-100 text-orange-800 dark:bg-orange-900 dark:text-orange-200',
		config: 'bg-cyan-100 text-cyan-800 dark:bg-cyan-900 dark:text-cyan-200',
		metric: 'bg-rose-100 text-rose-800 dark:bg-rose-900 dark:text-rose-200',
		other: 'bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300'
	};

	function categoryColor(cat: string): string {
		return categoryColors[cat.toLowerCase()] ?? categoryColors.other;
	}

	function formatBytes(bytes: number | null): string {
		if (bytes === null || bytes === undefined) return '--';
		if (bytes === 0) return '0 B';
		const units = ['B', 'KB', 'MB', 'GB', 'TB'];
		const i = Math.floor(Math.log(bytes) / Math.log(1024));
		return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
	}

	function formatTime(s: string): string {
		return new Intl.DateTimeFormat(undefined, {
			hour: '2-digit', minute: '2-digit', second: '2-digit'
		}).format(new Date(s));
	}

	function formatFullDate(s: string): string {
		return new Intl.DateTimeFormat(undefined, {
			month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit'
		}).format(new Date(s));
	}

	function relativeTime(from: string, to: string): string {
		const ms = new Date(to).getTime() - new Date(from).getTime();
		if (ms < 1000) return '<1s';
		if (ms < 60_000) return `${Math.round(ms / 1000)}s`;
		if (ms < 3600_000) {
			const m = Math.floor(ms / 60_000);
			const s = Math.round((ms % 60_000) / 1000);
			return s > 0 ? `${m}m ${s}s` : `${m}m`;
		}
		return `${Math.floor(ms / 3600_000)}h ${Math.round((ms % 3600_000) / 60_000)}m`;
	}

	function stepTime(step: { artifacts: { created_at: string }[] }): string | null {
		return step.artifacts.length > 0 ? step.artifacts[0].created_at : null;
	}

	// ── Load data ──────────────────────────────────────────────────────────────
	$effect(() => {
		const processId = $page.params.process_id;
		if (!processId) return;

		loading = true;
		error = null;

		getCatalogueLineage(processId)
			.then(async (data) => {
				lineage = data;
				if (highlightArtifact && browser) {
					await tick();
					const el = document.getElementById(`artifact-${highlightArtifact}`);
					el?.scrollIntoView({ behavior: 'smooth', block: 'center' });
				}
			})
			.catch((e) => {
				error = e instanceof Error ? e.message : 'Failed to load lineage';
			})
			.finally(() => {
				loading = false;
			});
	});
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-4xl px-6 py-8 animate-rise">

		<!-- Back link -->
		<a
			href="/data"
			class="mb-6 inline-flex items-center gap-1.5 text-sm text-muted-foreground transition-colors hover:text-foreground"
		>
			<ArrowLeft class="size-4" />
			Back to Data
		</a>

		<!-- Header -->
		{#if lineage}
			<div class="mb-6">
				<div class="flex items-center gap-2">
					<GitBranch class="size-6 text-muted-foreground" />
					<h1 class="text-2xl font-semibold tracking-tight text-foreground">
						Process Lineage
					</h1>
					{#if instanceId}
						<Button variant="outline" size="sm" href="/instances/{instanceId}/process" class="ml-auto gap-1.5">
							<Activity class="size-4" />
							Open instance
						</Button>
					{/if}
				</div>
				<p class="mt-1 font-mono text-sm text-muted-foreground">
					{lineage.process_id}
				</p>
				<div class="mt-2 flex items-center gap-4 text-sm text-muted-foreground">
					<span class="flex items-center gap-1.5">
						<FileBox class="size-4" />
						{lineage.total_artifacts} artifact{lineage.total_artifacts === 1 ? '' : 's'}
					</span>
					<span>{lineage.steps.length} step{lineage.steps.length === 1 ? '' : 's'}</span>
				</div>
			</div>

			<Separator class="mb-6" />

			<!-- Timeline -->
			<div class="relative">
				<!-- Vertical line -->
				<div class="absolute left-[88px] top-0 bottom-0 w-px bg-border"></div>

				<div class="space-y-6">
					{#each lineage.steps as step, idx}
						{@const ts = stepTime(step)}
						{@const prevTs = idx > 0 ? stepTime(lineage.steps[idx - 1]) : null}
						<div class="relative flex gap-0">
							<!-- Timestamp left of the line -->
							<div class="w-[80px] shrink-0 pt-3 pr-3 text-right">
								{#if ts}
									<p class="text-sm font-medium tabular-nums text-foreground">
										{formatTime(ts)}
									</p>
									{#if prevTs}
										<p class="text-sm text-muted-foreground">
											+{relativeTime(prevTs, ts)}
										</p>
									{:else}
										<p class="text-sm text-muted-foreground">
											{formatFullDate(ts)}
										</p>
									{/if}
								{/if}
							</div>

							<!-- Timeline dot -->
							<div class="absolute left-[84px] top-3.5 size-3 rounded-full border-2 border-primary bg-background"></div>

							<!-- Step card -->
							<div class="ml-4 flex-1 rounded-lg border border-border bg-card">
								<div class="flex items-center gap-3 px-4 py-3 border-b border-border">
									{#if step.iteration !== null}
										<span class="inline-flex size-6 items-center justify-center rounded-full bg-primary text-sm font-bold text-primary-foreground tabular-nums">
											{step.iteration}
										</span>
									{/if}
									<span class="text-sm font-semibold text-foreground">
										{step.step}
									</span>
									<span class="ml-auto text-sm text-muted-foreground">
										{step.artifacts.length} artifact{step.artifacts.length === 1 ? '' : 's'}
									</span>
								</div>

								<!-- Artifact rows -->
								<div class="space-y-0">
									{#each step.artifacts as artifact}
										<div id="artifact-{artifact.id}">
											<ArtifactCard
												entry={artifact}
												highlighted={highlightArtifact === artifact.id}
												expanded={expandedId === artifact.id}
												onToggle={() => { expandedId = expandedId === artifact.id ? null : artifact.id; }}
											/>
										</div>
									{/each}
								</div>
							</div>
						</div>
					{/each}
				</div>
			</div>

		{:else if loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading lineage...
			</div>

		{:else if error}
			<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
				{error}
			</div>
		{/if}

	</div>
</div>
