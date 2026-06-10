<script lang="ts">
	// Display name / slug / description editing for a folder. Body of the
	// Settings tab in the folders browser's detail pane. Delete lives on the
	// pane header, not here.
	import Save from '@lucide/svelte/icons/save';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { updateFolder, type Folder } from '$lib/api/client';

	type Props = {
		folder: Folder;
		/** A slug change re-roots the subtree's materialized paths — the parent reloads. */
		onUpdated: (f: Folder) => void;
	};
	let { folder, onUpdated }: Props = $props();

	// Draft state seeds from the folder ONCE — the parent `{#key}`s this panel
	// by folder.id, so a selection change remounts it with fresh seeds.
	// svelte-ignore state_referenced_locally
	let editName = $state(folder.display_name);
	// svelte-ignore state_referenced_locally
	let editDescription = $state(folder.description);
	// svelte-ignore state_referenced_locally
	let editSlug = $state(folder.slug);
	let saving = $state(false);
	let error = $state<string | null>(null);

	const dirty = $derived(
		editName.trim() !== folder.display_name ||
			editDescription !== folder.description ||
			editSlug.trim() !== folder.slug
	);

	async function save(e: Event) {
		e.preventDefault();
		if (!dirty || saving) return;
		saving = true;
		error = null;
		try {
			// A slug change is a MOVE (re-roots the materialized path).
			const next = await updateFolder(folder.id, {
				display_name: editName.trim() || folder.display_name,
				description: editDescription,
				slug: editSlug.trim() || folder.slug
			});
			editName = next.display_name;
			editDescription = next.description;
			editSlug = next.slug;
			onUpdated(next);
		} catch (err) {
			error = err instanceof Error ? err.message : 'Failed to save';
		} finally {
			saving = false;
		}
	}
</script>

<form onsubmit={save} class="space-y-3">
	<div class="grid gap-3 sm:grid-cols-3">
		<label class="space-y-1 text-sm">
			<span class="text-muted-foreground">Display name</span>
			<Input bind:value={editName} data-testid="input-folder-name" />
		</label>
		<label class="space-y-1 text-sm">
			<span class="text-muted-foreground">Slug</span>
			<Input bind:value={editSlug} data-testid="input-folder-slug" />
		</label>
		<label class="space-y-1 text-sm">
			<span class="text-muted-foreground">Description</span>
			<Input bind:value={editDescription} data-testid="input-folder-description" />
		</label>
	</div>
	<p class="text-xs text-muted-foreground">
		Changing the slug re-roots this folder's path (and every descendant's).
	</p>
	{#if error}
		<div class="text-sm text-destructive">{error}</div>
	{/if}
	<Button type="submit" size="sm" disabled={!dirty || saving} data-testid="btn-save-folder">
		<Save class="size-4" />
		{saving ? 'Saving…' : 'Save changes'}
	</Button>
</form>
