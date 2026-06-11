<script lang="ts">
	// Folder-tree rail for the standalone Resources / Assets browsers. Loads the
	// active workspace's folders and renders the same SideRail + FolderTree the
	// Folders page uses, with a synthetic root row that means "everything in the
	// workspace" (no folder scope). Selecting a folder scopes the page's list to
	// that folder's subtree; selecting the root clears the scope.
	//
	// Self-contained on folder loading so both pages share it without each
	// re-implementing the workspace/folder fetch. Selection + URL state stay with
	// the caller (it owns the deep-link contract).
	import { SideRail } from '$lib/components/shell';
	import FolderTreeRailSection from '$lib/components/folders/FolderTreeRailSection.svelte';
	import { workspaces } from '$lib/workspaces/store.svelte';
	import { listFolders, type Folder } from '$lib/api/client';

	let {
		rootLabel,
		selectedId,
		onSelect,
		testid = 'folder-scope-rail'
	}: {
		/** Label for the synthetic "no folder scope" root row. */
		rootLabel: string;
		selectedId: string | null;
		onSelect: (id: string | null) => void;
		testid?: string;
	} = $props();

	const workspaceId = $derived(workspaces.active?.id ?? '');
	let folders = $state<Folder[]>([]);

	let loadedFor = $state<string | null>(null);
	$effect(() => {
		workspaces.load();
		const ws = workspaceId;
		if (!ws || loadedFor === ws) return;
		loadedFor = ws;
		listFolders(ws)
			.then((f) => (folders = f))
			.catch(() => (folders = []));
	});
</script>

<SideRail {testid}>
	<div class="space-y-6 p-4">
		<FolderTreeRailSection {folders} {selectedId} {onSelect} {rootLabel} showRoot />
	</div>
</SideRail>
