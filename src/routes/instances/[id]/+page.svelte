<script lang="ts">
	import { page } from '$app/state';
	import { getInstance, getInstanceState, cancelInstance } from '$lib/api/client';
	import type { WorkflowInstance, InstanceState } from '$lib/types/api';
	import Activity from '@lucide/svelte/icons/activity';
	import RefreshCw from '@lucide/svelte/icons/refresh-cw';

	const instanceId = $derived(page.params.id!);

	let instance = $state<WorkflowInstance | null>(null);
	let instanceState = $state<InstanceState | null>(null);
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
			instance = await getInstance(instanceId);
			if (instance.status === 'running') {
				instanceState = await getInstanceState(instanceId);
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load instance';
		} finally {
			loading = false;
		}
	}

	async function refresh() {
		if (!instance) return;
		try {
			instance = await getInstance(instanceId);
			if (instance.status === 'running') {
				instanceState = await getInstanceState(instanceId);
			}
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to refresh';
		}
	}

	async function handleCancel() {
		if (!instance || !confirm('Cancel this instance?')) return;
		try {
			await cancelInstance(instance.id);
			instance = { ...instance, status: 'cancelled' };
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to cancel';
		}
	}

	const formatDate = (s: string | null) => (s ? new Date(s).toLocaleString() : '-');

	$effect(() => {
		load();
	});
</script>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-3xl px-6 py-8">
		{#if loading}
			<div class="flex items-center justify-center py-16 text-sm text-muted-foreground">
				Loading...
			</div>
		{:else if error}
			<div class="rounded-lg border border-amber-200 bg-amber-50 px-4 py-3 text-sm text-amber-800">
				{error}
			</div>
		{:else if instance}
			<div class="mb-6 flex items-start justify-between">
				<div>
					<div class="flex items-center gap-2">
						<h1 class="text-2xl font-semibold tracking-tight text-foreground">Instance</h1>
						<span class="rounded-full px-2.5 py-0.5 text-xs font-medium {statusColors[instance.status] ?? ''}">
							{instance.status}
						</span>
					</div>
					<p class="mt-1 font-mono text-xs text-muted-foreground">{instance.net_id}</p>
				</div>
				<div class="flex items-center gap-2">
					<button
						type="button"
						class="flex items-center gap-1.5 rounded-md border border-border px-3 py-1.5 text-xs text-foreground transition-colors hover:bg-accent"
						onclick={refresh}
					>
						<RefreshCw class="size-3.5" />
						Refresh
					</button>
					{#if instance.status === 'running' || instance.status === 'created'}
						<button
							type="button"
							class="rounded-md border border-destructive/30 px-3 py-1.5 text-xs text-destructive transition-colors hover:bg-destructive/10"
							onclick={handleCancel}
						>
							Cancel
						</button>
					{/if}
				</div>
			</div>

			<div class="space-y-4">
				<div class="rounded-lg border border-border bg-card">
					<div class="border-b border-border px-4 py-2.5">
						<span class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
							Details
						</span>
					</div>
					<dl class="divide-y divide-border">
						<div class="flex justify-between px-4 py-2.5">
							<dt class="text-xs text-muted-foreground">Instance ID</dt>
							<dd class="font-mono text-xs text-foreground">{instance.id}</dd>
						</div>
						<div class="flex justify-between px-4 py-2.5">
							<dt class="text-xs text-muted-foreground">Template</dt>
							<dd class="text-xs text-foreground">
								<a href="/templates/{instance.template_id}" class="text-primary underline">
									v{instance.template_version}
								</a>
							</dd>
						</div>
						<div class="flex justify-between px-4 py-2.5">
							<dt class="text-xs text-muted-foreground">Created</dt>
							<dd class="text-xs text-foreground">{formatDate(instance.created_at)}</dd>
						</div>
						<div class="flex justify-between px-4 py-2.5">
							<dt class="text-xs text-muted-foreground">Started</dt>
							<dd class="text-xs text-foreground">{formatDate(instance.started_at)}</dd>
						</div>
						<div class="flex justify-between px-4 py-2.5">
							<dt class="text-xs text-muted-foreground">Completed</dt>
							<dd class="text-xs text-foreground">{formatDate(instance.completed_at)}</dd>
						</div>
						{#if instance.current_step}
							<div class="flex justify-between px-4 py-2.5">
								<dt class="text-xs text-muted-foreground">Current Step</dt>
								<dd class="text-xs font-medium text-foreground">{instance.current_step}</dd>
							</div>
						{/if}
					</dl>
				</div>

				{#if instanceState}
					<div class="rounded-lg border border-border bg-card">
						<div class="border-b border-border px-4 py-2.5">
							<span class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
								Live State (Marking)
							</span>
						</div>
						<div class="px-4 py-3">
							{#each Object.entries(instanceState.marking) as [placeId, tokens] (placeId)}
								{@const tokenList = tokens as unknown[]}
								{#if tokenList.length > 0}
									<div class="mb-2 flex items-center gap-2">
										<div class="size-2 rounded-full bg-blue-500"></div>
										<span class="text-xs text-foreground">{placeId}</span>
										<span class="text-[10px] text-muted-foreground">
											({tokenList.length} token{tokenList.length !== 1 ? 's' : ''})
										</span>
									</div>
								{/if}
							{/each}
						</div>
					</div>

					{#if instanceState.enabled_transitions.length > 0}
						<div class="rounded-lg border border-border bg-card">
							<div class="border-b border-border px-4 py-2.5">
								<span class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
									Enabled Transitions
								</span>
							</div>
							<div class="px-4 py-3">
								{#each instanceState.enabled_transitions as transitionId (transitionId)}
									<div class="mb-1 flex items-center gap-2">
										<Activity class="size-3 text-amber-500" />
										<span class="text-xs text-foreground">{transitionId}</span>
									</div>
								{/each}
							</div>
						</div>
					{/if}
				{/if}
			</div>
		{/if}
	</div>
</div>
