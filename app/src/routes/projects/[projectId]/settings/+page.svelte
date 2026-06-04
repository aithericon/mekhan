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
	import { updateProject, deleteProject } from '$lib/api/client';
	import { getProjectContext } from '$lib/components/projects/project-context';

	const ctx = getProjectContext();
	const project = $derived(ctx.project);

	let editName = $state('');
	let editDescription = $state('');
	let savingSettings = $state(false);
	let settingsError = $state<string | null>(null);
	let hydrated = $state(false);

	// Seed the form from the loaded project once it arrives.
	$effect(() => {
		if (project && !hydrated) {
			editName = project.display_name;
			editDescription = project.description;
			hydrated = true;
		}
	});

	const dirty = $derived(
		!!project && (editName.trim() !== project.display_name || editDescription !== project.description)
	);

	async function saveSettings(e: Event) {
		e.preventDefault();
		if (!project || !dirty) return;
		savingSettings = true;
		settingsError = null;
		try {
			// Assigning ctx.project updates the shared header in the layout too.
			ctx.project = await updateProject(project.id, {
				display_name: editName.trim() || project.display_name,
				description: editDescription
			});
			editName = ctx.project.display_name;
			editDescription = ctx.project.description;
		} catch (err) {
			settingsError = err instanceof Error ? err.message : 'Failed to save';
		} finally {
			savingSettings = false;
		}
	}

	async function handleDelete() {
		if (!project) return;
		if (!confirm(`Delete project '${project.display_name}'? This cannot be undone.`)) return;
		try {
			await deleteProject(project.id);
			goto('/projects');
		} catch (e) {
			alert(e instanceof Error ? e.message : 'Failed to delete project');
		}
	}
</script>

<Card>
	<CardHeader>
		<CardTitle>Settings</CardTitle>
		<CardDescription>Rename this project or update its description. The slug is immutable.</CardDescription>
	</CardHeader>
	<CardContent>
		{#if project}
			<form onsubmit={saveSettings} class="space-y-3">
				<div class="grid gap-3 sm:grid-cols-2">
					<label class="space-y-1 text-sm">
						<span class="text-muted-foreground">Display name</span>
						<Input bind:value={editName} data-testid="input-project-name" />
					</label>
					<label class="space-y-1 text-sm">
						<span class="text-muted-foreground">Description</span>
						<Input bind:value={editDescription} data-testid="input-project-description" />
					</label>
				</div>
				{#if settingsError}
					<div class="text-sm text-destructive">{settingsError}</div>
				{/if}
				<div class="flex items-center justify-between">
					<Button type="submit" size="sm" disabled={!dirty || savingSettings} data-testid="btn-save-project">
						<Save class="size-4" />
						{savingSettings ? 'Saving…' : 'Save changes'}
					</Button>
					<Button
						type="button"
						variant="outline"
						size="sm"
						class="text-destructive hover:text-destructive"
						onclick={handleDelete}
						data-testid="btn-delete-project"
					>
						<Trash2 class="size-4" /> Delete project
					</Button>
				</div>
			</form>
		{:else}
			<p class="text-sm text-muted-foreground">Loading…</p>
		{/if}
	</CardContent>
</Card>
