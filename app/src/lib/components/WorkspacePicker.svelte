<script lang="ts">
	import { onMount } from 'svelte';
	import Building from '@lucide/svelte/icons/building';
	import Check from '@lucide/svelte/icons/check';
	import Eye from '@lucide/svelte/icons/eye';
	import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down';
	import Cog from '@lucide/svelte/icons/cog';
	import Plus from '@lucide/svelte/icons/plus';
	import { Button } from '$lib/components/ui/button';
	import {
		DropdownMenu,
		DropdownMenuTrigger,
		DropdownMenuContent,
		DropdownMenuItem,
		DropdownMenuSeparator
	} from '$lib/components/ui/dropdown-menu';
	import { workspaces } from '$lib/workspaces/store.svelte';
	import CreateWorkspaceDialog from '$lib/workspaces/CreateWorkspaceDialog.svelte';

	let createOpen = $state(false);

	onMount(() => {
		workspaces.load();
	});

	async function switchTo(id: string) {
		if (workspaces.active?.id === id) return;
		try {
			await workspaces.switchTo(id);
		} catch (err) {
			console.error('failed to switch workspace', err);
		}
	}

	const active = $derived(workspaces.active);
	const list = $derived(workspaces.workspaces);
</script>

<DropdownMenu>
	<DropdownMenuTrigger>
		<Button
			variant="ghost"
			size="sm"
			class="gap-1.5 text-muted-foreground"
			data-testid="workspace-picker-trigger"
			title="Switch active workspace"
		>
			<Building class="size-3.5" />
			<span class="max-w-[14ch] truncate text-sm">
				{active?.display_name ?? (workspaces.loaded ? 'No workspace' : '…')}
			</span>
			<ChevronsUpDown class="size-3 opacity-60" />
		</Button>
	</DropdownMenuTrigger>
	<DropdownMenuContent class="min-w-[16rem]" align="end">
		{#if list.length === 0 && workspaces.loaded}
			<div class="px-2 py-1.5 text-sm text-muted-foreground">
				You aren't a member of any workspace yet.
			</div>
		{/if}
		{#each list as ws (ws.id)}
			<DropdownMenuItem
				onclick={() => switchTo(ws.id)}
				data-testid={`workspace-option-${ws.slug}`}
			>
				<div class="flex w-full items-center gap-2">
					<div class="min-w-0 flex-1">
						<div class="flex items-center gap-1.5">
							<span class="truncate text-sm">{ws.display_name}</span>
							{#if !ws.my_role}
								<!-- Browse-only (e.g. the demos catalogue): you're not a member,
								     so it's read-only — fork content out to edit/run it. -->
								<span
									class="inline-flex items-center gap-0.5 rounded bg-muted px-1 py-0.5 text-[10px] font-medium text-muted-foreground"
									title="Read-only — fork content into a workspace you own"
								>
									<Eye class="size-2.5" /> read-only
								</span>
							{/if}
						</div>
						<div class="truncate text-xs text-muted-foreground">{ws.slug}</div>
					</div>
					{#if active?.id === ws.id}
						<Check class="size-4 text-foreground" />
					{/if}
				</div>
			</DropdownMenuItem>
		{/each}
		<DropdownMenuSeparator />
		<DropdownMenuItem onclick={() => (createOpen = true)} data-testid="workspace-create-item">
			<Plus class="mr-2 size-3.5" />
			<span class="text-sm">New workspace</span>
		</DropdownMenuItem>
		{#if active}
			<DropdownMenuItem onclick={() => (window.location.href = `/workspaces/${active.id}`)}>
				<Cog class="mr-2 size-3.5" />
				<span class="text-sm">Manage workspace</span>
			</DropdownMenuItem>
		{/if}
	</DropdownMenuContent>
</DropdownMenu>

<CreateWorkspaceDialog bind:open={createOpen} />
