<script lang="ts">
	import { createTaskStore } from '$lib/stores/tasks.svelte';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import ClipboardList from '@lucide/svelte/icons/clipboard-list';

	const store = createTaskStore();

	type StatusConfig = { class: string; label: string };
	const statusConfig: Record<string, StatusConfig> = {
		pending: { class: 'border-amber-200 bg-amber-50 text-amber-700', label: 'Pending' },
		completed: { class: 'border-emerald-200 bg-emerald-50 text-emerald-700', label: 'Completed' },
		cancelled: { class: 'border-slate-200 bg-slate-50 text-slate-600', label: 'Cancelled' },
		failed: { class: 'border-red-200 bg-red-50 text-red-600', label: 'Rejected' }
	};

	const hoverByStatus: Record<string, string> = {
		pending: 'hover:border-primary/40 hover:shadow-md',
		completed: 'hover:border-emerald-300 hover:bg-emerald-50/40',
		cancelled: 'hover:border-slate-300 hover:bg-slate-50/40',
		failed: 'hover:border-red-300 hover:bg-red-50/40'
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

	function formatDate(s?: string | null): string {
		return s ? new Date(s).toLocaleString() : '—';
	}

	function formatDuration(ms?: number | null): string | null {
		if (ms == null) return null;
		if (ms < 1000) return `${ms}ms`;
		const seconds = Math.floor(ms / 1000);
		if (seconds < 60) return `${(ms / 1000).toFixed(1)}s`;
		const minutes = Math.floor(seconds / 60);
		const remSec = seconds % 60;
		if (minutes < 60) return `${minutes}m ${remSec}s`;
		const hours = Math.floor(minutes / 60);
		const remMin = minutes % 60;
		if (hours < 24) return `${hours}h ${remMin}m`;
		const days = Math.floor(hours / 24);
		const remHours = hours % 24;
		return `${days}d ${remHours}h`;
	}

	function dateText(task: { status: string; created_at: string; completed_at?: string; cancelled_at?: string; failed_at?: string }): string {
		switch (task.status) {
			case 'completed':
				return `Completed ${formatDate(task.completed_at ?? task.created_at)}`;
			case 'cancelled':
				return `Cancelled ${formatDate(task.cancelled_at ?? task.created_at)}`;
			case 'failed':
				return `Rejected ${formatDate(task.failed_at ?? task.created_at)}`;
			default:
				return `Received ${formatDate(task.created_at)}`;
		}
	}

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
			{#each filters as f (f.key)}
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
				<p class="text-sm text-muted-foreground">
					Tasks appear when workflows reach human task steps
				</p>
			</div>
		{:else}
			<div class="space-y-2">
				{#each store.tasks as task (task.task_id)}
					{@const cfg = statusConfig[task.status] ?? statusConfig.pending}
					{@const hover = hoverByStatus[task.status] ?? hoverByStatus.pending}
					{@const duration = formatDuration(task.duration_ms)}
					<a
						href="/tasks/{task.task_id}"
						class="group block rounded-xl border border-border bg-card p-4 transition {hover}"
					>
						<div class="flex items-start justify-between gap-3">
							<div class="min-w-0 flex-1">
								<div class="truncate text-sm font-semibold leading-snug text-foreground">
									{task.title}
								</div>
								<div class="mt-1.5 flex flex-wrap items-center gap-x-1.5 gap-y-1 text-sm text-muted-foreground">
									<span>{dateText(task)}</span>
									{#if duration}
										<span class="text-muted-foreground/60">·</span>
										<span>{duration}</span>
									{/if}
									{#if task.process_id}
										<span class="text-muted-foreground/60">·</span>
										<span class="font-mono text-sm">{task.process_id}</span>
									{/if}
								</div>
							</div>
							<Badge variant="outline" class="shrink-0 rounded-full {cfg.class}">
								{cfg.label}
							</Badge>
						</div>
					</a>
				{/each}
			</div>

			{#if store.total > store.tasks.length}
				<p class="mt-4 text-center text-sm text-muted-foreground">
					Showing {store.tasks.length} of {store.total} tasks
				</p>
			{/if}
		{/if}
	</div>
</div>
