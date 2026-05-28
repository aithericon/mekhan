<script lang="ts">
	import { onMount } from 'svelte';
	import Building from '@lucide/svelte/icons/building';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import { Card, CardContent, CardHeader, CardTitle } from '$lib/components/ui/card';
	import { Badge } from '$lib/components/ui/badge';
	import { workspaces } from '$lib/workspaces/store.svelte';

	onMount(() => workspaces.load());

	const list = $derived(workspaces.workspaces);
</script>

<div class="mx-auto max-w-3xl px-6 py-8" data-testid="workspaces-index">
	<h1 class="text-2xl font-semibold tracking-tight">Workspaces</h1>
	<p class="mt-1 text-sm text-muted-foreground">
		Every workspace you're a member of. Click one to manage members, projects, and tags.
	</p>

	{#if !workspaces.loaded}
		<div class="mt-6 text-sm text-muted-foreground">Loading…</div>
	{:else if list.length === 0}
		<div class="mt-6 rounded-lg border border-dashed border-border p-6 text-sm text-muted-foreground">
			You don't belong to any workspace yet. Ask an admin to add you.
		</div>
	{:else}
		<div class="mt-6 space-y-2" data-testid="workspaces-list">
			{#each list as ws (ws.id)}
				<a
					href={`/workspaces/${ws.id}`}
					class="flex items-center gap-3 rounded-lg border border-border bg-card p-4 hover:bg-accent/50"
					data-testid={`workspace-link-${ws.slug}`}
				>
					<Building class="size-5 text-muted-foreground" />
					<div class="min-w-0 flex-1">
						<div class="flex items-center gap-2">
							<span class="font-medium">{ws.display_name}</span>
							{#if ws.is_system}
								<Badge variant="secondary" class="text-xs">system</Badge>
							{/if}
						</div>
						<div class="text-sm text-muted-foreground">{ws.slug}</div>
					</div>
					<ArrowRight class="size-4 text-muted-foreground" />
				</a>
			{/each}
		</div>
	{/if}
</div>
