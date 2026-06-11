<script lang="ts">
	// Assets browser. Full-width sidebar-page anatomy (same as /folders and
	// /resources): a folder-tree rail on the left scopes the asset types +
	// collections list, the body is `AssetList.svelte` for the selected folder
	// (or the whole workspace at the root). Create/edit flows live in AssetList.
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { PageShell, PageHeader } from '$lib/components/shell';
	import FolderScopeRail from '$lib/components/folders/FolderScopeRail.svelte';
	import { AssetList } from '$lib/components/assets';
	import type { ScopeContext } from '$lib/api/assets';

	// Folder scope is URL-addressable (`/assets?folder=<id>`), matching /folders
	// and /resources.
	let selectedFolderId = $state<string | null>(page.url.searchParams.get('folder'));

	const scope = $derived<ScopeContext>(
		selectedFolderId ? { kind: 'folder', id: selectedFolderId } : { kind: 'workspace' }
	);

	function selectFolder(id: string | null) {
		selectedFolderId = id;
		goto(id ? `/assets?folder=${id}` : '/assets', {
			replaceState: true,
			keepFocus: true,
			noScroll: true
		});
	}
</script>

<PageShell width="full" testid="assets-page">
	{#snippet band()}
		<PageHeader
			title="Assets"
			subtitle="Curated, user-typed static content (material parameters, scripts, reference data). Define a schema, populate records via the table builder or CSV, and bind collections into workflow nodes as ordinary staged inputs."
		/>
	{/snippet}

	{#snippet sidebar()}
		<FolderScopeRail
			rootLabel="All assets"
			selectedId={selectedFolderId}
			onSelect={selectFolder}
			testid="assets-folder-rail"
		/>
	{/snippet}

	<AssetList {scope} />
</PageShell>
