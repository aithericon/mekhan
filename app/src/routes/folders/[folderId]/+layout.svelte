<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import ChevronLeft from '@lucide/svelte/icons/chevron-left';
	import Settings from '@lucide/svelte/icons/settings';
	import { Badge } from '$lib/components/ui/badge';
	import { Button } from '$lib/components/ui/button';
	import { PageShell, PageHeader, PageTabs } from '$lib/components/shell';
	import { workspaces } from '$lib/workspaces/store.svelte';
	import { listFolders } from '$lib/api/client';
	import {
		provideFolderContext,
		type FolderContext
	} from '$lib/components/folders/folder-context';

	let { children } = $props();

	const folderId = $derived(page.params.folderId ?? '');
	const pathname = $derived(page.url.pathname);

	// Single reactive handle shared with every subroute. Subpages mutate
	// `folder` via `reload()` (or by assigning after a save); the object
	// reference itself stays stable.
	const ctx = $state<FolderContext>({
		folderId: '', // set from the route in $effect below
		workspaceId: '',
		folder: null,
		loading: true,
		error: null,
		reload
	});

	provideFolderContext(ctx);

	async function reload() {
		ctx.loading = true;
		ctx.error = null;
		try {
			await workspaces.load();
			const wsId = workspaces.active?.id ?? '';
			ctx.workspaceId = wsId;
			if (!wsId) {
				ctx.error = 'No active workspace. Pick one from the workspace switcher first.';
				return;
			}
			// Folders are scoped to the active workspace (same as the list page).
			const folders = await listFolders(wsId);
			ctx.folder = folders.find((f) => f.id === ctx.folderId) ?? null;
			if (!ctx.folder) {
				ctx.error = 'Folder not found in the active workspace';
			} else {
				ctx.workspaceId = ctx.folder.workspace_id;
			}
		} catch (e) {
			ctx.error = e instanceof Error ? e.message : 'Failed to load folder';
		} finally {
			ctx.loading = false;
		}
	}

	$effect(() => {
		ctx.folderId = folderId;
		if (folderId) reload();
	});

	// Tab nav drives the subroutes (PageTabs link-tabs). Settings lives behind
	// the gear — neither tab's href prefix matches /settings, so no tab is
	// highlighted there (the gear button shows the active state instead).
	const onSettings = $derived(pathname.endsWith('/settings'));
</script>

<svelte:head><title>{ctx.folder?.display_name ?? 'Folder'} | Mekhan</title></svelte:head>

<PageShell>
	{#if ctx.loading && !ctx.folder}
		<a
			href="/folders"
			class="mb-3 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
		>
			<ChevronLeft class="size-4" /> Folders
		</a>
		<p class="text-sm text-muted-foreground">Loading…</p>
	{:else if ctx.error && !ctx.folder}
		<a
			href="/folders"
			class="mb-3 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
		>
			<ChevronLeft class="size-4" /> Folders
		</a>
		<div class="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
			{ctx.error}
		</div>
	{:else if ctx.folder}
		{@const folder = ctx.folder}
		<!-- Static folder header — shared across every tab subroute. -->
		<PageHeader
			title={folder.display_name}
			variant="detail"
			back={{ href: '/folders', label: 'Folders' }}
			headTitle={false}
			class="mb-5"
		>
			<div class="mt-1 flex items-center gap-2">
				<Badge variant="secondary" class="font-mono text-sm">{folder.path}</Badge>
				{#if folder.description}
					<span class="truncate text-sm text-muted-foreground">{folder.description}</span>
				{/if}
			</div>
			{#snippet actions()}
				<Button
					variant={onSettings ? 'secondary' : 'ghost'}
					size="sm"
					title="Folder settings"
					onclick={() => goto(`/folders/${ctx.folderId}/settings`)}
					data-testid="btn-folder-settings"
				>
					<Settings class="size-4" /> Settings
				</Button>
			{/snippet}
		</PageHeader>

		<PageTabs
			class="mb-5"
			tabs={[
				{ href: `/folders/${ctx.folderId}/templates`, label: 'Templates', testid: 'tab-templates' },
				{ href: `/folders/${ctx.folderId}/api`, label: 'API', testid: 'tab-api' }
			]}
		/>

		{@render children()}
	{/if}
</PageShell>
