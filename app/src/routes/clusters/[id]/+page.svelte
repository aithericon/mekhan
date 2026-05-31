<script lang="ts">
	import { page } from '$app/state';
	import { PoolContentionView } from '$lib/components/petri';
	import { Button } from '$lib/components/ui/button';
	import { Badge } from '$lib/components/ui/badge';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import RotateCcw from '@lucide/svelte/icons/rotate-ccw';
	import {
		listClusters,
		reconnectCluster,
		drainCluster,
		type ClusterSummary
	} from '$lib/api/clusters';

	const resourceId = $derived(page.params.id ?? '');
	const poolNetId = $derived(`pool-${resourceId}`);

	let cluster = $state<ClusterSummary | null>(null);
	let busy = $state(false);
	let error = $state<string | null>(null);

	async function load() {
		try {
			const all = await listClusters();
			cluster = all.find((c) => c.resource_id === resourceId) ?? null;
			error = null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load cluster';
		}
	}

	$effect(() => {
		void resourceId;
		load();
		const t = setInterval(load, 4000);
		return () => clearInterval(t);
	});

	async function act(fn: (id: string) => Promise<unknown>) {
		busy = true;
		try {
			await fn(resourceId);
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Action failed';
		} finally {
			busy = false;
		}
	}

	const name = $derived(
		cluster?.display_name || cluster?.resource_path || resourceId
	);
</script>

<svelte:head><title>{name} | Clusters | Mekhan</title></svelte:head>

<div class="h-full overflow-y-auto">
	<div class="mx-auto max-w-6xl px-6 py-8 animate-rise">
		<a
			href="/clusters"
			class="mb-3 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
		>
			<ChevronLeft class="size-4" /> Clusters
		</a>

		<div class="mb-4 flex items-start justify-between gap-3">
			<div>
				<h1 class="text-lg font-semibold tracking-tight">{name}</h1>
				<div class="mt-1 flex items-center gap-2 text-sm">
					{#if cluster}
						<Badge variant="secondary">{cluster.flavor}</Badge>
						<Badge variant="secondary">{cluster.watcher_state}</Badge>
						<span class="font-mono text-xs text-muted-foreground">{poolNetId}</span>
					{:else}
						<span class="font-mono text-xs text-muted-foreground">{poolNetId}</span>
					{/if}
				</div>
			</div>
			{#if cluster && cluster.resource_id !== '_env'}
				<div class="flex items-center gap-1.5">
					<Button variant="outline" size="sm" disabled={busy} onclick={() => act(reconnectCluster)}>
						<RotateCcw class="mr-1.5 size-4" /> Reconnect
					</Button>
					<Button
						variant="outline"
						size="sm"
						disabled={busy || cluster.draining}
						onclick={() => act(drainCluster)}
					>
						Drain
					</Button>
				</div>
			{/if}
		</div>

		{#if error}
			<div class="mb-3 rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
				{error}
			</div>
		{/if}

		{#key poolNetId}
			<PoolContentionView netId={poolNetId} />
		{/key}
	</div>
</div>
