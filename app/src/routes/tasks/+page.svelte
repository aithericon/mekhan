<script lang="ts">
	import { createTaskStore } from '$lib/stores/tasks.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import ClipboardList from '@lucide/svelte/icons/clipboard-list';

	const store = createTaskStore();

	const statusColors: Record<string, string> = {
		pending: 'bg-amber-100 text-amber-700',
		completed: 'bg-green-100 text-green-700',
		cancelled: 'bg-slate-100 text-slate-700',
		failed: 'bg-red-100 text-red-700'
	};

	type StatusFilter = 'all' | 'pending' | 'completed' | 'failed';
	let activeFilter: StatusFilter = $state('all');

	const filters: { key: StatusFilter; label: string }[] = [
		{ key: 'all', label: 'All' },
		{ key: 'pending', label: 'Pending' },
		{ key: 'completed', label: 'Completed' },
		{ key: 'failed', label: 'Failed' }
	];

	function setFilter(f: StatusFilter) {
		activeFilter = f;
		store.refetch(f === 'all' ? undefined : f);
	}

	const formatDate = (s: string) => new Date(s).toLocaleString();

	$effect(() => {
		store.init();
		return () => store.destroy();
	});
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-5xl px-6 py-8 animate-rise">
		<div class="mb-6">
			<h1 class="text-2xl font-semibold tracking-tight text-foreground">Tasks</h1>
			<p class="mt-1 text-sm text-muted-foreground">
				Human tasks from running workflows
			</p>
		</div>

		<!-- Status filter tabs -->
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

		{#if store.error}
			<div class="mb-4 rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
				{store.error}
			</div>
		{/if}

		{#if store.loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading...
			</div>
		{:else if store.tasks.length === 0}
			<div
				class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16"
			>
				<ClipboardList class="size-10 text-muted-foreground/40" />
				<p class="mt-3 text-sm text-muted-foreground">No tasks</p>
				<p class="text-xs text-muted-foreground">
					Tasks appear when workflows reach human task steps
				</p>
			</div>
		{:else}
			<div class="space-y-2">
				{#each store.tasks as task (task.task_id)}
					<a
						href="/tasks/{task.task_id}"
						class="group flex items-center justify-between rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/50"
					>
						<div class="min-w-0">
							<div class="flex items-center gap-2">
								<span class="text-sm font-medium text-foreground truncate">
									{task.title}
								</span>
								<Badge
									class={statusColors[task.status] ?? ''}
									variant="secondary"
								>
									{task.status}
								</Badge>
							</div>
							{#if task.process_id}
								<p class="mt-1 text-xs text-muted-foreground">
									Process: {task.process_id}
								</p>
							{/if}
							<p class="mt-1 text-[10px] text-muted-foreground">
								{formatDate(task.created_at)}
								{#if task.duration_ms}
									<span class="ml-2">({(task.duration_ms / 1000).toFixed(1)}s)</span>
								{/if}
							</p>
						</div>
					</a>
				{/each}
			</div>

			{#if store.total > store.tasks.length}
				<p class="mt-4 text-center text-xs text-muted-foreground">
					Showing {store.tasks.length} of {store.total} tasks
				</p>
			{/if}
		{/if}
	</div>
</div>
