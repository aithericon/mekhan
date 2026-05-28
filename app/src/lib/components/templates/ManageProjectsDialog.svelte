<script lang="ts">
	import * as Dialog from '$lib/components/ui/dialog';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import FolderKanban from '@lucide/svelte/icons/folder-kanban';
	import Plus from '@lucide/svelte/icons/plus';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import {
		listProjects,
		createProject,
		updateProject,
		deleteProject,
		type Project
	} from '$lib/api/client';
	import { workspaces } from '$lib/workspaces/store.svelte';

	interface Props {
		open: boolean;
		/** Fired after any create/rename/delete so the parent can refresh filters. */
		onChanged?: () => void;
	}

	let { open = $bindable(), onChanged }: Props = $props();

	let projects = $state<Project[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);

	let newName = $state('');
	let creating = $state(false);

	let renamingId = $state<string | null>(null);
	let renameDraft = $state('');
	let renameRef = $state<HTMLInputElement | null>(null);

	// Derive a Rhai-safe-ish slug from the display name. The backend enforces
	// per-workspace uniqueness and rejects malformed slugs, so a clash surfaces
	// as a 409 inline rather than silently overwriting.
	function slugify(name: string): string {
		return (
			name
				.toLowerCase()
				.trim()
				.replace(/[^a-z0-9]+/g, '-')
				.replace(/^-+|-+$/g, '')
				.slice(0, 48) || 'project'
		);
	}

	async function loadProjects() {
		const ws = workspaces.active?.id;
		if (!ws) {
			projects = [];
			return;
		}
		loading = true;
		error = null;
		try {
			projects = await listProjects(ws);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load projects';
			projects = [];
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (open) {
			error = null;
			renamingId = null;
			newName = '';
			loadProjects();
		}
	});

	$effect(() => {
		if (renamingId) renameRef?.focus();
	});

	async function create() {
		const ws = workspaces.active?.id;
		const name = newName.trim();
		if (!ws || !name || creating) return;
		creating = true;
		error = null;
		try {
			const p = await createProject(ws, {
				slug: slugify(name),
				display_name: name,
				description: ''
			});
			projects = [...projects, p];
			newName = '';
			onChanged?.();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to create project';
		} finally {
			creating = false;
		}
	}

	function startRename(p: Project) {
		renamingId = p.id;
		renameDraft = p.display_name;
	}

	async function commitRename() {
		const id = renamingId;
		if (!id) return;
		renamingId = null;
		const p = projects.find((x) => x.id === id);
		if (!p) return;
		const next = renameDraft.trim();
		if (!next || next === p.display_name) return;
		const prev = p.display_name;
		projects = projects.map((x) => (x.id === id ? { ...x, display_name: next } : x)); // optimistic
		try {
			await updateProject(id, { display_name: next });
			onChanged?.();
		} catch (e) {
			projects = projects.map((x) => (x.id === id ? { ...x, display_name: prev } : x));
			error = e instanceof Error ? e.message : 'Rename failed';
		}
	}

	function onRenameKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			e.preventDefault();
			commitRename();
		} else if (e.key === 'Escape') {
			e.preventDefault();
			renamingId = null;
		}
	}

	async function remove(p: Project) {
		if (!confirm(`Delete project “${p.display_name}”? Templates are unaffected — only the grouping is removed.`))
			return;
		const prev = projects;
		projects = projects.filter((x) => x.id !== p.id); // optimistic
		error = null;
		try {
			await deleteProject(p.id);
			onChanged?.();
		} catch (e) {
			projects = prev;
			error = e instanceof Error ? e.message : 'Delete failed';
		}
	}

	function onNewKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			e.preventDefault();
			create();
		}
	}
</script>

<Dialog.Root bind:open>
	<Dialog.Content class="sm:max-w-lg" data-testid="manage-projects-dialog">
		<Dialog.Header>
			<Dialog.Title>Manage projects</Dialog.Title>
			<Dialog.Description>
				Projects group templates within {workspaces.active?.display_name ?? 'this workspace'}.
				They are labels, not a permission boundary.
			</Dialog.Description>
		</Dialog.Header>

		<div class="space-y-4">
			<!-- Existing projects -->
			{#if loading}
				<div class="py-6 text-center text-sm text-muted-foreground">Loading projects…</div>
			{:else if projects.length === 0}
				<div class="rounded-md border border-dashed border-border py-6 text-center text-sm text-muted-foreground">
					No projects yet
				</div>
			{:else}
				<ul class="divide-y divide-border rounded-md border border-border" data-testid="manage-projects-list">
					{#each projects as p (p.id)}
						<li class="flex items-center gap-2 px-3 py-2" data-testid={`manage-project-${p.slug}`}>
							<FolderKanban class="size-4 shrink-0 text-muted-foreground" />
							{#if renamingId === p.id}
								<Input
									bind:ref={renameRef}
									bind:value={renameDraft}
									onkeydown={onRenameKeydown}
									onblur={commitRename}
									onfocus={(e) => (e.currentTarget as HTMLInputElement).select()}
									aria-label="Project name"
									data-testid={`manage-project-rename-input-${p.slug}`}
									class="h-7 flex-1 text-sm"
								/>
							{:else}
								<span class="flex-1 truncate text-sm text-foreground">{p.display_name}</span>
								<span class="shrink-0 font-mono text-xs text-muted-foreground/60">{p.slug}</span>
							{/if}
							<Button
								variant="ghost"
								size="sm"
								class="size-7 p-0"
								aria-label={`Rename ${p.display_name}`}
								onclick={() => startRename(p)}
								data-testid={`manage-project-rename-${p.slug}`}
							>
								<Pencil class="size-3.5" />
							</Button>
							<Button
								variant="ghost"
								size="sm"
								class="size-7 p-0 text-muted-foreground hover:text-destructive"
								aria-label={`Delete ${p.display_name}`}
								onclick={() => remove(p)}
								data-testid={`manage-project-delete-${p.slug}`}
							>
								<Trash2 class="size-3.5" />
							</Button>
						</li>
					{/each}
				</ul>
			{/if}

			<!-- Add project -->
			<div class="flex items-center gap-2">
				<Input
					placeholder="New project name…"
					bind:value={newName}
					onkeydown={onNewKeydown}
					data-testid="manage-project-new-input"
					class="flex-1"
				/>
				<Button
					onclick={create}
					disabled={!newName.trim() || creating}
					data-testid="manage-project-create"
				>
					<Plus class="size-4" />
					{creating ? 'Adding…' : 'Add'}
				</Button>
			</div>

			{#if error}
				<div class="text-sm text-destructive" data-testid="manage-projects-error">{error}</div>
			{/if}
		</div>
	</Dialog.Content>
</Dialog.Root>
