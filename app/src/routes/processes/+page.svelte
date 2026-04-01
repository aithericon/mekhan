<script lang="ts">
	import { listProcesses } from '$lib/api/client';
	import type { ProcessState } from '$lib/types/tasks';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import Workflow from '@lucide/svelte/icons/workflow';

	let processes: ProcessState[] = $state([]);
	let loading = $state(true);
	let error: string | null = $state(null);

	const statusColors: Record<string, string> = {
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700'
	};

	type StatusFilter = 'all' | 'running' | 'completed' | 'failed';
	let activeFilter: StatusFilter = $state('all');

	const filters: { key: StatusFilter; label: string }[] = [
		{ key: 'all', label: 'All' },
		{ key: 'running', label: 'Running' },
		{ key: 'completed', label: 'Completed' },
		{ key: 'failed', label: 'Failed' }
	];

	async function load(status?: string) {
		loading = true;
		error = null;
		try {
			const result = await listProcesses({
				status: status === 'all' ? undefined : status,
				limit: 100
			});
			processes = result.processes ?? [];
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load processes';
			processes = [];
		} finally {
			loading = false;
		}
	}

	function setFilter(f: StatusFilter) {
		activeFilter = f;
		load(f);
	}

	function progressOf(p: ProcessState): { completed: number; total: number } {
		const total = p.timeline?.length ?? 0;
		const completed = p.timeline?.filter((e) => e.status === 'completed').length ?? 0;
		return { completed, total };
	}

	const formatDate = (s: string) => new Date(s).toLocaleString();

	$effect(() => {
		load();
	});
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">
		<div class="mb-6">
			<h1 class="text-2xl font-semibold tracking-tight text-foreground">Processes</h1>
			<p class="mt-1 text-sm text-muted-foreground">
				Workflow processes with step-by-step tracking
			</p>
		</div>

		<div class="mb-4 flex gap-1">
			{#each filters as f}
				<Button
					variant={activeFilter === f.key ? 'default' : 'ghost'}
					size="sm"
					onclick={() => setFilter(f.key)}
				>
					{f.label}
				</Button>
			{/each}
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
		{:else if processes.length === 0}
			<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
				<Workflow class="size-10 text-muted-foreground/40" />
				<p class="mt-3 text-sm text-muted-foreground">No processes</p>
				<p class="text-xs text-muted-foreground">
					Processes appear when workflows with process tracking run
				</p>
			</div>
		{:else}
			<div class="space-y-2">
				{#each processes as proc (proc.process_id)}
					{@const progress = progressOf(proc)}
					<a
						href="/processes/{proc.process_id}"
						class="group flex items-center justify-between rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/50"
					>
						<div class="min-w-0 flex-1">
							<div class="flex items-center gap-2">
								<span class="text-sm font-medium text-foreground truncate">
									{proc.name}
								</span>
								<Badge class={statusColors[proc.status] ?? ''} variant="secondary">
									{proc.status}
								</Badge>
							</div>
							{#if proc.current_step}
								<p class="mt-1 text-xs text-muted-foreground">
									Current: {proc.current_step}
								</p>
							{/if}
							<p class="mt-1 text-[10px] text-muted-foreground">
								{formatDate(proc.started_at)}
								{#if proc.duration_ms}
									<span class="ml-2">({(proc.duration_ms / 1000).toFixed(1)}s)</span>
								{/if}
							</p>
						</div>
						<!-- Progress bar -->
						<div class="ml-4 flex items-center gap-2">
							<div class="h-1.5 w-24 rounded-full bg-muted">
								<div
									class="h-full rounded-full transition-all {proc.status === 'failed' ? 'bg-red-500' : 'bg-primary'}"
									style="width: {progress.total > 0 ? (progress.completed / progress.total) * 100 : 0}%"
								></div>
							</div>
							<span class="text-xs tabular-nums text-muted-foreground">
								{progress.completed}/{progress.total}
							</span>
						</div>
					</a>
				{/each}
			</div>
		{/if}
	</div>
</div>
