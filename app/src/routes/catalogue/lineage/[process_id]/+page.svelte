<script lang="ts">
	import { page } from '$app/stores';
	import { getCatalogueLineage, catalogueDownloadUrl } from '$lib/api/client';
	import type { LineageResponse } from '$lib/types/catalogue';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { Separator } from '$lib/components/ui/separator';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import Download from '@lucide/svelte/icons/download';
	import FileBox from '@lucide/svelte/icons/file-box';
	import GitBranch from '@lucide/svelte/icons/git-branch';

	// ── State ──────────────────────────────────────────────────────────────────
	let lineage = $state<LineageResponse | null>(null);
	let loading = $state(true);
	let error = $state<string | null>(null);

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

	// ── Load data ──────────────────────────────────────────────────────────────
	$effect(() => {
		const processId = $page.params.process_id;
		if (!processId) return;

		loading = true;
		error = null;

		getCatalogueLineage(processId)
			.then((data) => {
				lineage = data;
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
			href="/catalogue"
			class="mb-6 inline-flex items-center gap-1.5 text-sm text-muted-foreground transition-colors hover:text-foreground"
		>
			<ArrowLeft class="size-4" />
			Back to catalogue
		</a>

		<!-- Header -->
		{#if lineage}
			<div class="mb-6">
				<div class="flex items-center gap-2">
					<GitBranch class="size-6 text-muted-foreground" />
					<h1 class="text-2xl font-semibold tracking-tight text-foreground">
						Process Lineage
					</h1>
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
				<div class="absolute left-4 top-0 bottom-0 w-px bg-border"></div>

				<div class="space-y-6">
					{#each lineage.steps as step, idx}
						<div class="relative pl-10">
							<!-- Timeline dot -->
							<div class="absolute left-2.5 top-3 size-3 rounded-full border-2 border-primary bg-background"></div>

							<!-- Step card -->
							<div class="rounded-lg border border-border bg-card">
								<div class="flex items-center gap-3 px-4 py-3 border-b border-border">
									<span class="text-sm font-semibold text-foreground">
										{step.step}
									</span>
									{#if step.iteration !== null}
										<Badge variant="outline" class="text-xs tabular-nums">
											Iteration {step.iteration}
										</Badge>
									{/if}
									<span class="ml-auto text-xs text-muted-foreground">
										{step.artifacts.length} artifact{step.artifacts.length === 1 ? '' : 's'}
									</span>
								</div>

								<!-- Artifact rows -->
								<div class="divide-y divide-border">
									{#each step.artifacts as artifact}
										<div class="flex items-center justify-between gap-4 px-4 py-2.5">
											<div class="min-w-0 flex-1">
												<div class="flex items-center gap-2">
													<span class="truncate text-sm font-medium text-foreground">
														{artifact.name}
													</span>
													<Badge class={categoryColor(artifact.category)} variant="secondary">
														{artifact.category}
													</Badge>
												</div>
												{#if artifact.filename}
													<p class="mt-0.5 truncate text-xs font-mono text-muted-foreground">
														{artifact.filename}
													</p>
												{/if}
											</div>

											<div class="flex shrink-0 items-center gap-3">
												<span class="text-xs tabular-nums text-muted-foreground">
													{formatBytes(artifact.size_bytes)}
												</span>
												{#if artifact.storage_path}
													<a
														href={catalogueDownloadUrl(artifact.storage_path)}
														class="inline-flex size-7 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
														download
														title="Download"
													>
														<Download class="size-3.5" />
													</a>
												{/if}
											</div>
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
