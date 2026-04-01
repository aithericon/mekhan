<script lang="ts">
	import { page } from '$app/stores';
	import { getProcess } from '$lib/api/client';
	import type { ProcessState } from '$lib/types/tasks';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import ArrowLeft from '@lucide/svelte/icons/arrow-left';
	import CheckCircle from '@lucide/svelte/icons/check-circle';
	import Circle from '@lucide/svelte/icons/circle';
	import Loader from '@lucide/svelte/icons/loader';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import User from '@lucide/svelte/icons/user';

	let process: ProcessState | null = $state(null);
	let loading = $state(true);
	let error: string | null = $state(null);

	const processId = $derived($page.params.id as string);

	const statusColors: Record<string, string> = {
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700'
	};

	const stepStatusColors: Record<string, string> = {
		pending: 'text-muted-foreground/40',
		running: 'text-blue-500',
		completed: 'text-green-500',
		failed: 'text-red-500'
	};

	function progress(p: ProcessState): { completed: number; total: number; percent: number } {
		const total = p.timeline?.length ?? 0;
		const completed = p.timeline?.filter((e) => e.status === 'completed').length ?? 0;
		return { completed, total, percent: total > 0 ? (completed / total) * 100 : 0 };
	}

	const formatDate = (s: string) => new Date(s).toLocaleString();
	const formatDuration = (ms: number) => {
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
		return `${Math.floor(ms / 60_000)}m ${Math.round((ms % 60_000) / 1000)}s`;
	};

	async function load() {
		loading = true;
		error = null;
		try {
			process = await getProcess(processId);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load process';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		processId;
		load();
	});
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-3xl px-6 py-8 animate-rise">
		<div class="mb-4">
			<Button variant="ghost" size="sm" href="/processes" class="gap-1 text-muted-foreground">
				<ArrowLeft class="size-4" />
				Back to processes
			</Button>
		</div>

		{#if error}
			<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
				{error}
			</div>
		{/if}

		{#if loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading...
			</div>
		{:else if process}
			{@const prog = progress(process)}

			<!-- Header -->
			<div class="rounded-xl border border-border bg-card p-5 mb-4">
				<div class="flex items-center gap-2 mb-2">
					<h1 class="text-lg font-semibold text-foreground">{process.name}</h1>
					<Badge class={statusColors[process.status] ?? ''} variant="secondary">
						{process.status}
					</Badge>
				</div>
				{#if process.description}
					<p class="text-sm text-muted-foreground mb-3">{process.description}</p>
				{/if}

				<!-- Progress bar -->
				<div class="flex items-center gap-3">
					<div class="h-2 flex-1 rounded-full bg-muted">
						<div
							class="h-full rounded-full transition-all {process.status === 'failed' ? 'bg-red-500' : 'bg-primary'}"
							style="width: {prog.percent}%"
						></div>
					</div>
					<span class="text-xs tabular-nums text-muted-foreground">
						{prog.completed}/{prog.total} steps
					</span>
				</div>

				<div class="mt-3 text-[10px] text-muted-foreground">
					Started {formatDate(process.started_at)}
					{#if process.completed_at}
						<span class="ml-2">Completed {formatDate(process.completed_at)}</span>
					{/if}
					{#if process.duration_ms}
						<span class="ml-2">({formatDuration(process.duration_ms)})</span>
					{/if}
				</div>

				{#if process.error}
					<div class="mt-3 rounded-md border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700">
						{process.error}
					</div>
				{/if}
			</div>

			<!-- Timeline -->
			<div class="rounded-xl border border-border bg-card p-5">
				<h2 class="text-sm font-medium text-foreground mb-4">Timeline</h2>
				<div class="space-y-0">
					{#each process.timeline as entry, i (entry.step)}
						<div class="flex gap-3 {i < process.timeline.length - 1 ? 'pb-4' : ''}">
							<!-- Step indicator with line -->
							<div class="flex flex-col items-center">
								<div class={stepStatusColors[entry.status] ?? ''}>
									{#if entry.status === 'completed'}
										<CheckCircle class="size-5" />
									{:else if entry.status === 'running'}
										<Loader class="size-5 animate-spin" />
									{:else if entry.status === 'failed'}
										<XCircle class="size-5" />
									{:else}
										<Circle class="size-5" />
									{/if}
								</div>
								{#if i < process.timeline.length - 1}
									<div class="mt-1 w-px flex-1 bg-border"></div>
								{/if}
							</div>

							<!-- Step content -->
							<div class="min-w-0 flex-1 pb-1">
								<div class="flex items-center gap-2">
									<span class="text-sm font-medium text-foreground">{entry.label}</span>
									{#if entry.human}
										<User class="size-3 text-muted-foreground" />
									{/if}
								</div>

								{#if entry.detail}
									<p class="mt-0.5 text-xs text-muted-foreground">{entry.detail}</p>
								{/if}

								{#if entry.progress_message}
									<p class="mt-0.5 text-xs text-muted-foreground">
										{entry.progress_message}
										{#if entry.progress_percent != null}
											({Math.round(entry.progress_percent)}%)
										{/if}
									</p>
								{/if}

								{#if entry.task_id}
									<a
										href="/tasks/{entry.task_id}"
										class="mt-1 inline-block text-xs text-primary hover:underline"
									>
										View task
									</a>
								{/if}

								<div class="mt-1 text-[10px] text-muted-foreground">
									{#if entry.started_at}
										{formatDate(entry.started_at)}
									{/if}
									{#if entry.duration_ms}
										<span class="ml-1">({formatDuration(entry.duration_ms)})</span>
									{/if}
								</div>
							</div>
						</div>
					{/each}
				</div>
			</div>
		{:else}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Process not found
			</div>
		{/if}
	</div>
</div>
