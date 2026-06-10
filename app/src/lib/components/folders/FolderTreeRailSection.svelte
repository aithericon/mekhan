<script lang="ts">
	// The "Folders" section of a SideRail — ONE header + tree anatomy shared by
	// the Folders browser and the Templates filter rail so the two trees read
	// identically. The header's right slot carries the page-appropriate
	// affordance (manage link, new-folder toggle), `extra` slots between header
	// and tree (create form), `children` renders below the tree (filter
	// toggles). Data and selection stay with the caller.
	import FolderTreeIcon from '@lucide/svelte/icons/folder-tree';
	import type { Snippet } from 'svelte';
	import FolderTree from '$lib/components/FolderTree.svelte';
	import type { Folder } from '$lib/api/client';

	let {
		folders,
		selectedId,
		onSelect,
		actions,
		headerAction,
		extra,
		rootLabel,
		showRoot,
		children
	}: {
		folders: Folder[];
		selectedId: string | null;
		onSelect: (id: string | null) => void;
		/** Per-row hover actions (manager pages). */
		actions?: Snippet<[Folder]>;
		/** Right-aligned affordance in the section header. */
		headerAction?: Snippet;
		/** Rendered between the header and the tree (e.g. a create form). */
		extra?: Snippet;
		rootLabel?: string;
		showRoot?: boolean;
		/** Rendered below the tree (e.g. filter toggles). */
		children?: Snippet;
	} = $props();
</script>

<section>
	<div class="mb-2 flex items-center justify-between gap-2">
		<div class="flex items-center gap-2 text-sm font-medium text-foreground">
			<FolderTreeIcon class="size-4 text-muted-foreground" />
			Folders
		</div>
		{#if headerAction}
			{@render headerAction()}
		{/if}
	</div>
	{#if extra}
		{@render extra()}
	{/if}
	<FolderTree {folders} {selectedId} {onSelect} {actions} {rootLabel} {showRoot} />
	{#if children}
		{@render children()}
	{/if}
</section>
