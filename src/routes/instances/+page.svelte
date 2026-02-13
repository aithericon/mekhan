<script lang="ts">
	import { listInstances, cancelInstance } from '$lib/api/client';
	import type { WorkflowInstance } from '$lib/types/api';
	import Activity from '@lucide/svelte/icons/activity';

	let instances = $state<WorkflowInstance[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);

	const statusColors: Record<string, string> = {
		created: 'bg-gray-100 text-gray-700',
		running: 'bg-blue-100 text-blue-700',
		completed: 'bg-green-100 text-green-700',
		failed: 'bg-red-100 text-red-700',
		cancelled: 'bg-slate-100 text-slate-700'
	};

	async function load() {
		loading = true;
		error = null;
		try {
			const result = await listInstances();
			instances = result.items;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load instances';
			instances = [];
		} finally {
			loading = false;
		}
	}

	async function handleCancel(id: string) {
		if (!confirm('Cancel this instance?')) return;
		try {
			await cancelInstance(id);
			instances = instances.map((i) => (i.id === id ? { ...i, status: 'cancelled' } : i));
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to cancel';
		}
	}

	const formatDate = (s: string) => new Date(s).toLocaleString();

	$effect(() => {
		load();
	});
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-5xl px-6 py-8">
		<div class="mb-6">
			<h1 class="text-2xl font-semibold tracking-tight text-foreground">Instances</h1>
			<p class="mt-1 text-sm text-muted-foreground">
				Running and completed workflow instances
			</p>
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
		{:else if instances.length === 0}
			<div class="flex flex-col items-center justify-center rounded-xl border border-dashed border-border py-16">
				<Activity class="size-10 text-muted-foreground/40" />
				<p class="mt-3 text-sm text-muted-foreground">No instances yet</p>
				<p class="text-xs text-muted-foreground">
					Publish a template and run it to create instances
				</p>
			</div>
		{:else}
			<div class="space-y-2">
				{#each instances as instance (instance.id)}
					<a
						href="/instances/{instance.id}"
						class="group flex items-center justify-between rounded-lg border border-border bg-card p-4 transition-colors hover:bg-accent/50"
					>
						<div class="min-w-0">
							<div class="flex items-center gap-2">
								<span class="font-mono text-xs text-foreground">{instance.net_id}</span>
								<span class="rounded-full px-2 py-0.5 text-[10px] font-medium {statusColors[instance.status] ?? ''}">
									{instance.status}
								</span>
							</div>
							{#if instance.current_step}
								<p class="mt-1 text-xs text-muted-foreground">
									Current: {instance.current_step}
								</p>
							{/if}
							<p class="mt-1 text-[10px] text-muted-foreground">
								Created {formatDate(instance.created_at)}
							</p>
						</div>
						{#if instance.status === 'running' || instance.status === 'created'}
							<button
								type="button"
								class="rounded px-2 py-1 text-xs text-muted-foreground opacity-0 transition-all hover:bg-destructive/10 hover:text-destructive group-hover:opacity-100"
								onclick={(e) => {
									e.preventDefault();
									e.stopPropagation();
									handleCancel(instance.id);
								}}
							>
								Cancel
							</button>
						{/if}
					</a>
				{/each}
			</div>
		{/if}
	</div>
</div>
