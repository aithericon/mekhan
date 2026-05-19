<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import type { components } from '$lib/api/schema';

	type FireResult = components['schemas']['FireResult'];

	let { nodeId }: { nodeId: string } = $props();

	let history = $state<FireResult[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);
	let timer: ReturnType<typeof setInterval> | null = null;

	onMount(() => {
		void fetchHistory();
		// Poll every 5s while the inspector is open — cheap (returns in-memory
		// data) and lets users see new fires without manually refreshing.
		timer = setInterval(() => void fetchHistory(), 5000);
	});
	onDestroy(() => {
		if (timer) clearInterval(timer);
	});

	async function fetchHistory() {
		if (!nodeId) return;
		loading = true;
		try {
			const res = await fetch(`/api/triggers/${encodeURIComponent(nodeId)}/history`);
			if (!res.ok) {
				error = `History fetch failed: ${res.status}`;
				return;
			}
			const body = await res.json();
			history = body.history ?? [];
			error = null;
		} catch (e) {
			error = String(e);
		} finally {
			loading = false;
		}
	}

	function fmt(iso: string): string {
		try {
			return new Date(iso).toLocaleString(undefined, {
				dateStyle: 'short',
				timeStyle: 'medium'
			});
		} catch {
			return iso;
		}
	}

	function outcomeLabel(r: FireResult): string {
		const o = r.outcome as { outcome: string; instance_id?: string; delivered_to?: number; reason?: string };
		switch (o.outcome) {
			case 'spawned':
				return `Spawned ${o.instance_id?.slice(0, 8)}…`;
			case 'signaled':
				return `Signaled ${o.delivered_to ?? 0} instance${(o.delivered_to ?? 0) === 1 ? '' : 's'}`;
			case 'no_targets':
				return 'No targets';
			case 'dropped':
				return `Dropped: ${o.reason ?? 'unknown'}`;
			default:
				return o.outcome;
		}
	}
</script>

<div class="space-y-2">
	<div class="flex items-center justify-between">
		<span class="text-sm font-medium text-muted-foreground">Recent fires</span>
		{#if loading}
			<span class="text-sm text-muted-foreground/70">…</span>
		{/if}
	</div>
	{#if error}
		<p class="text-sm text-destructive">{error}</p>
	{:else if history.length === 0}
		<p class="rounded-md border border-dashed border-border/50 p-2 text-sm text-muted-foreground">
			No fires yet.
		</p>
	{:else}
		<ul class="max-h-48 space-y-1 overflow-y-auto">
			{#each history.slice().reverse() as result, i (i)}
				<li class="rounded-md border border-border/60 bg-muted/20 px-2 py-1.5">
					<div class="flex items-center justify-between gap-2">
						<span class="text-sm text-muted-foreground">{fmt(result.fired_at)}</span>
						<span class="text-sm font-medium">{outcomeLabel(result)}</span>
					</div>
				</li>
			{/each}
		</ul>
	{/if}
</div>
