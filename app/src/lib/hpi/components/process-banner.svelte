<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import type { HumanTask, ProcessState } from '../types';

	let {
		process,
		task,
		processHref = null
	}: {
		process: ProcessState;
		task: HumanTask;
		processHref?: string | null;
	} = $props();

	const completedSteps = $derived(
		(process.timeline ?? []).filter((e) => e.status === 'completed').length
	);
	const totalSteps = $derived((process.timeline ?? []).length);
	const progressPercent = $derived(
		totalSteps > 0 ? Math.round((completedSteps / totalSteps) * 100) : 0
	);
	// Hide the progress bar + step counter when there's no timeline data
	// — the upstream `ProcessDetail` shape doesn't always carry `timeline`
	// and a "Step 0 of 0" badge with an empty bar is worse than nothing.
	// We still render the banner row so the process name stays clickable.
	const hasProgress = $derived(totalSteps > 0);
</script>

{#snippet bannerContent()}
	<div
		class="flex size-8 shrink-0 items-center justify-center rounded-lg bg-cyan-100 text-cyan-700"
	>
		<svg class="size-4" viewBox="0 0 16 16" fill="none">
			<path
				d="M2 4h12M2 8h12M2 12h12"
				stroke="currentColor"
				stroke-width="1.5"
				stroke-linecap="round"
			/>
		</svg>
	</div>
	<div class="min-w-0 flex-1">
		<div class="flex items-center gap-2">
			<span class="truncate text-sm font-medium text-foreground">{process.name}</span>
			{#if task.process_step}
				<span class="shrink-0 text-sm text-muted-foreground">· {task.process_step}</span>
			{/if}
		</div>
		{#if hasProgress}
			<div class="mt-1 flex items-center gap-2">
				<div class="h-1 w-24 overflow-hidden rounded-full bg-cyan-200/50">
					<div class="h-full rounded-full bg-cyan-500" style={`width: ${progressPercent}%`}></div>
				</div>
				<span class="text-sm text-muted-foreground">
					Step {completedSteps} of {totalSteps}
				</span>
			</div>
		{/if}
	</div>
	{#if processHref}
		<ArrowRight class="size-4 shrink-0 text-muted-foreground/60" />
	{/if}
{/snippet}

{#if processHref}
	<a
		href={processHref}
		data-testid="process-context-banner"
		data-print="hide"
		class="mb-4 flex items-center gap-3 rounded-xl border border-cyan-200 bg-cyan-50/60 p-3 transition-colors hover:bg-cyan-50"
	>
		{@render bannerContent()}
	</a>
{:else}
	<div class="mb-4 flex items-center gap-3 rounded-xl border border-cyan-200 bg-cyan-50/60 p-3">
		{@render bannerContent()}
	</div>
{/if}
