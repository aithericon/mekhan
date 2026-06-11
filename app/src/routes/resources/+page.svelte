<script lang="ts">
	// Resources browser. Full-width sidebar-page anatomy (same as /folders): a
	// folder-tree rail on the left scopes the list, the body is the resource
	// list for the selected folder (or the whole workspace at the root). The
	// list view + create/edit sheet live in `ResourceList.svelte`.
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import { workspaces } from '$lib/workspaces/store.svelte';
	import FolderScopeRail from '$lib/components/folders/FolderScopeRail.svelte';
	import { ResourceList } from '$lib/components/resources';

	const workspaceId = $derived(workspaces.active?.id ?? '');

	// Folder scope is URL-addressable (`/resources?folder=<id>`) so a folder's
	// resource view can be deep-linked, matching the /folders contract.
	let selectedFolderId = $state<string | null>(page.url.searchParams.get('folder'));

	function selectFolder(id: string | null) {
		selectedFolderId = id;
		goto(id ? `/resources?folder=${id}` : '/resources', {
			replaceState: true,
			keepFocus: true,
			noScroll: true
		});
	}
</script>

<PageShell width="full" testid="resources-page">
	{#snippet band()}
		<PageHeader
			title="Resources"
			subtitle="Typed credentials workflows bind by alias at launch (Postgres, OpenAI, S3, Slack, …). Secrets live in Vault; only public fields are listed here."
		/>
	{/snippet}

	{#snippet sidebar()}
		<FolderScopeRail
			rootLabel="All resources"
			selectedId={selectedFolderId}
			onSelect={selectFolder}
			testid="resources-folder-rail"
		/>
	{/snippet}

	<ResourceList workspace_id={workspaceId || undefined} folderId={selectedFolderId ?? undefined} />
</PageShell>
