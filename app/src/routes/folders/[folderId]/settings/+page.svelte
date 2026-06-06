<script lang="ts">
	import { goto } from '$app/navigation';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import Save from '@lucide/svelte/icons/save';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import {
		Card,
		CardHeader,
		CardTitle,
		CardContent,
		CardDescription
	} from '$lib/components/ui/card';
	import { updateFolder, deleteFolder } from '$lib/api/client';
	import { getFolderContext } from '$lib/components/folders/folder-context';

	const ctx = getFolderContext();
	const folder = $derived(ctx.folder);

	let editName = $state('');
	let editDescription = $state('');
	let editSlug = $state('');
	let savingSettings = $state(false);
	let settingsError = $state<string | null>(null);
	let hydrated = $state(false);

	// Seed the form from the loaded folder once it arrives.
	$effect(() => {
		if (folder && !hydrated) {
			editName = folder.display_name;
			editDescription = folder.description;
			editSlug = folder.slug;
			hydrated = true;
		}
	});

	const dirty = $derived(
		!!folder &&
			(editName.trim() !== folder.display_name ||
				editDescription !== folder.description ||
				editSlug.trim() !== folder.slug)
	);

	async function saveSettings(e: Event) {
		e.preventDefault();
		if (!folder || !dirty) return;
		savingSettings = true;
		settingsError = null;
		try {
			// A slug change is a MOVE (re-roots the materialized path). Assigning
			// ctx.folder updates the shared header in the layout too.
			ctx.folder = await updateFolder(folder.id, {
				display_name: editName.trim() || folder.display_name,
				description: editDescription,
				slug: editSlug.trim() || folder.slug
			});
			editName = ctx.folder.display_name;
			editDescription = ctx.folder.description;
			editSlug = ctx.folder.slug;
		} catch (err) {
			settingsError = err instanceof Error ? err.message : 'Failed to save';
		} finally {
			savingSettings = false;
		}
	}

	async function handleDelete() {
		if (!folder) return;
		if (!confirm(`Delete folder '${folder.display_name}'? Child folders are reparented to its parent.`))
			return;
		try {
			await deleteFolder(folder.id);
			goto('/folders');
		} catch (e) {
			alert(e instanceof Error ? e.message : 'Failed to delete folder');
		}
	}
</script>

<Card>
	<CardHeader>
		<CardTitle>Settings</CardTitle>
		<CardDescription>Rename this folder, edit its description, or change its slug (a slug change re-roots the path).</CardDescription>
	</CardHeader>
	<CardContent>
		{#if folder}
			<form onsubmit={saveSettings} class="space-y-3">
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
				{#if settingsError}
					<div class="text-sm text-destructive">{settingsError}</div>
				{/if}
				<div class="flex items-center justify-between">
					<Button type="submit" size="sm" disabled={!dirty || savingSettings} data-testid="btn-save-folder">
						<Save class="size-4" />
						{savingSettings ? 'Saving…' : 'Save changes'}
					</Button>
					<Button
						type="button"
						variant="outline"
						size="sm"
						class="text-destructive hover:text-destructive"
						onclick={handleDelete}
						data-testid="btn-delete-folder"
					>
						<Trash2 class="size-4" /> Delete folder
					</Button>
				</div>
			</form>
		{:else}
			<p class="text-sm text-muted-foreground">Loading…</p>
		{/if}
	</CardContent>
</Card>
