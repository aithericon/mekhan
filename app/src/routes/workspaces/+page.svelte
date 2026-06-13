<script lang="ts">
	import { onMount } from 'svelte';
	import Building from '@lucide/svelte/icons/building';
	import ArrowRight from '@lucide/svelte/icons/arrow-right';
	import Plus from '@lucide/svelte/icons/plus';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { workspaces } from '$lib/workspaces/store.svelte';
	import CreateWorkspaceDialog from '$lib/workspaces/CreateWorkspaceDialog.svelte';

	let createOpen = $state(false);

	onMount(() => workspaces.load());

	const list = $derived(workspaces.workspaces);
</script>

<PageShell testid="workspaces-index">
	{#snippet band()}
		<PageHeader
			title="Workspaces"
			subtitle="Every workspace you're a member of. Click one to manage members, projects, and tags."
		>
			{#snippet actions()}
				<Button size="sm" onclick={() => (createOpen = true)} data-testid="workspaces-new-button">
					<Plus class="mr-1.5 size-4" />
					New workspace
				</Button>
			{/snippet}
		</PageHeader>
	{/snippet}

	{#if !workspaces.loaded}
		<div class="text-sm text-muted-foreground">Loading…</div>
	{:else if list.length === 0}
		<div
			class="flex flex-col items-start gap-3 rounded-lg border border-dashed border-border p-6 text-sm text-muted-foreground"
		>
			<p>You don't belong to any workspace yet. Create one to get started.</p>
			<Button size="sm" onclick={() => (createOpen = true)} data-testid="workspaces-empty-new-button">
				<Plus class="mr-1.5 size-4" />
				New workspace
			</Button>
		</div>
	{:else}
		<div class="space-y-2" data-testid="workspaces-list">
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
</PageShell>

<CreateWorkspaceDialog bind:open={createOpen} />
