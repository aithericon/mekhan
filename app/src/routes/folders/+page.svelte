<script lang="ts">
	import Plus from '@lucide/svelte/icons/plus';
	import Copy from '@lucide/svelte/icons/copy';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import Pencil from '@lucide/svelte/icons/pencil';
	import FolderInput from '@lucide/svelte/icons/folder-input';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import BookOpen from '@lucide/svelte/icons/book-open';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Select from '$lib/components/ui/select';
	import { workspaces } from '$lib/workspaces/store.svelte';
	import {
		listFolders,
		createFolder,
		updateFolder,
		deleteFolder,
		type Folder
	} from '$lib/api/client';
	import FolderTree from '$lib/components/FolderTree.svelte';

	// Top-level folders are scoped to the active workspace — same implicit
	// scoping every other top-level page (Templates, Instances, …) uses.
	const workspaceId = $derived(workspaces.active?.id ?? '');

	let folders = $state<Folder[]>([]);
	let loading = $state(true);
	let error = $state<string | null>(null);
	let selectedId = $state<string | null>(null);

	const selected = $derived(folders.find((f) => f.id === selectedId) ?? null);

	// New-folder form. Parent defaults to whatever folder is selected in the tree.
	let newSlug = $state('');
	let newName = $state('');
	let newParentId = $state<string | null>(null);
	let creating = $state(false);
	let createError = $state<string | null>(null);

	// Inline rename of the selected folder.
	let renaming = $state(false);
	let renameDraft = $state('');

	// Move panel for the selected folder.
	let moving = $state(false);
	let moveParentId = $state<string | null>(null);
	let moveBusy = $state(false);

	// Root uses a sentinel (bits-ui Select reserves the empty string).
	const ROOT = '__root__';

	const sorted = $derived([...folders].sort((a, b) => a.path.localeCompare(b.path)));

	function moveTargets(excludeId: string | null): Folder[] {
		if (!excludeId) return sorted;
		const self = folders.find((f) => f.id === excludeId);
		const prefix = self ? `${self.path}/` : '';
		return sorted.filter(
			(f) => f.id !== excludeId && !(self && (f.path === self.path || f.path.startsWith(prefix)))
		);
	}

	async function load() {
		if (!workspaceId) return;
		loading = true;
		error = null;
		try {
			folders = await listFolders(workspaceId);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load folders';
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		workspaces.load();
		if (workspaceId) load();
	});

	// Keep the new-folder parent in lockstep with the tree selection.
	$effect(() => {
		newParentId = selectedId;
	});

	function selectFolder(id: string | null) {
		selectedId = id;
		renaming = false;
		moving = false;
	}

	// Inline "new subfolder" affordance from a tree row: select the folder (which
	// mirrors into the create form's parent) and focus the slug input.
	function startSubfolder(parent: Folder) {
		selectFolder(parent.id);
		newSlug = '';
		newName = '';
		queueMicrotask(() => {
			document
				.querySelector<HTMLInputElement>('[data-testid="input-new-folder-slug"]')
				?.focus();
		});
	}

	async function handleCreate(e: Event) {
		e.preventDefault();
		const slug = newSlug.trim();
		const display = newName.trim() || slug;
		if (!slug || !workspaceId) return;
		creating = true;
		createError = null;
		try {
			const f = await createFolder(workspaceId, {
				slug,
				display_name: display,
				description: '',
				parent_id: newParentId ?? undefined
			});
			folders = [...folders, f];
			newSlug = '';
			newName = '';
		} catch (err) {
			createError = err instanceof Error ? err.message : 'Failed to create folder';
		} finally {
			creating = false;
		}
	}

	function startRename() {
		if (!selected) return;
		renameDraft = selected.display_name;
		moving = false;
		renaming = true;
	}

	async function commitRename() {
		const f = selected;
		if (!f) return;
		const next = renameDraft.trim();
		renaming = false;
		if (!next || next === f.display_name) return;
		const prev = f.display_name;
		folders = folders.map((x) => (x.id === f.id ? { ...x, display_name: next } : x)); // optimistic
		try {
			await updateFolder(f.id, { display_name: next });
		} catch (e) {
			folders = folders.map((x) => (x.id === f.id ? { ...x, display_name: prev } : x));
			error = e instanceof Error ? e.message : 'Rename failed';
		}
	}

	function startMove() {
		if (!selected) return;
		moveParentId = selected.parent_id ?? null;
		renaming = false;
		moving = true;
	}

	async function commitMove() {
		const f = selected;
		if (!f || moveBusy) return;
		moveBusy = true;
		error = null;
		try {
			await updateFolder(f.id, { parent_id: moveParentId });
			moving = false;
			await load(); // materialized paths shift across the subtree — reload.
		} catch (e) {
			error = e instanceof Error ? e.message : 'Move failed';
		} finally {
			moveBusy = false;
		}
	}

	async function handleDelete() {
		const f = selected;
		if (!f) return;
		if (
			!confirm(
				`Delete folder “${f.display_name}”? Child folders are reparented to this folder's parent; templates are unaffected.`
			)
		)
			return;
		try {
			await deleteFolder(f.id);
			selectedId = f.parent_id ?? null;
			await load();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to delete folder';
		}
	}

	function bundleUrl(f: Folder): string {
		return `/api/v1/workspaces/${f.workspace_id}/folders/${f.id}/openapi.json`;
	}

	async function copyBundleUrl(f: Folder) {
		const url = `${window.location.origin}${bundleUrl(f)}`;
		try {
			await navigator.clipboard.writeText(url);
		} catch {
			prompt('Copy this URL', url);
		}
	}

	const newParentLabel = $derived(
		newParentId ? (folders.find((f) => f.id === newParentId)?.path ?? '—') : 'Workspace root'
	);
	const moveParentLabel = $derived(
		moveParentId ? (folders.find((f) => f.id === moveParentId)?.path ?? '—') : 'Workspace root'
	);
</script>

<svelte:head><title>Folders | Mekhan</title></svelte:head>

{#snippet actions(f: Folder)}
	<button
		type="button"
		class="flex size-5 items-center justify-center rounded text-muted-foreground hover:bg-background hover:text-foreground"
		title={`New subfolder under ${f.display_name}`}
		aria-label={`New subfolder under ${f.display_name}`}
		onclick={(e) => {
			e.stopPropagation();
			startSubfolder(f);
		}}
		data-testid={`btn-new-subfolder-${f.slug}`}
	>
		<Plus class="size-3.5" />
	</button>
{/snippet}

<div class="mx-auto max-w-5xl px-6 py-8" data-testid="folders-index">
	<header class="mb-6 flex items-baseline justify-between">
		<div>
			<h1 class="text-2xl font-semibold tracking-tight">Folders</h1>
			<p class="mt-1 text-sm text-muted-foreground">
				Organize templates into a hierarchy. Each folder gets its own per-webhook OpenAPI
				bundle.{#if workspaces.active}
					Workspace: <span class="font-medium">{workspaces.active.display_name}</span>.{/if}
			</p>
		</div>
	</header>

	{#if !workspaceId}
		<div class="rounded-lg border border-dashed border-border p-6 text-sm text-muted-foreground">
			No active workspace. Pick one from the workspace switcher first.
		</div>
	{:else}
		<form
			onsubmit={handleCreate}
			class="mb-6 flex flex-wrap items-end gap-2 rounded-lg border border-border bg-card/50 p-4"
		>
			<label class="flex-1 space-y-1 text-sm">
				<span class="text-muted-foreground">Slug</span>
				<Input placeholder="slug" bind:value={newSlug} data-testid="input-new-folder-slug" />
			</label>
			<label class="flex-1 space-y-1 text-sm">
				<span class="text-muted-foreground">Display name</span>
				<Input placeholder="Display name" bind:value={newName} data-testid="input-new-folder-name" />
			</label>
			<label class="flex-1 space-y-1 text-sm">
				<span class="text-muted-foreground">Parent</span>
				<Select.Root
					type="single"
					value={newParentId ?? ROOT}
					onValueChange={(v) => (newParentId = v === ROOT ? null : (v ?? null))}
				>
					<Select.Trigger class="h-9 w-full text-sm" data-testid="select-new-folder-parent">
						<span class="truncate">{newParentLabel}</span>
					</Select.Trigger>
					<Select.Content>
						<Select.Item value={ROOT} label="Workspace root" />
						{#each sorted as t (t.id)}
							<Select.Item value={t.id} label={t.path} />
						{/each}
					</Select.Content>
				</Select.Root>
			</label>
			<Button type="submit" disabled={creating} data-testid="btn-create-folder">
				<Plus class="size-4" />
				{creating ? 'Creating…' : 'Create folder'}
			</Button>
		</form>
		{#if createError}
			<div class="mb-4 text-sm text-destructive">{createError}</div>
		{/if}
		{#if error}
			<div class="mb-4 rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
				{error}
			</div>
		{/if}

		{#if loading}
			<p class="text-sm text-muted-foreground">Loading…</p>
		{:else}
			<div class="grid gap-4 md:grid-cols-[18rem_1fr]">
				<!-- Tree -->
				<aside class="rounded-lg border border-border bg-card/30 p-3" data-testid="folders-tree-panel">
					<FolderTree {folders} {selectedId} onSelect={selectFolder} {actions} />
				</aside>

				<!-- Detail -->
				<section class="min-w-0 rounded-lg border border-border bg-card/50 p-4" data-testid="folder-detail">
					{#if !selected}
						<div class="flex h-full items-center justify-center py-12 text-center text-sm text-muted-foreground">
							Select a folder from the tree to manage it, or create one above.
						</div>
					{:else}
						<div class="space-y-4">
							<div class="flex items-start justify-between gap-2">
								<div class="min-w-0">
									{#if renaming}
										<Input
											bind:value={renameDraft}
											onblur={commitRename}
											onkeydown={(e) => {
												if (e.key === 'Enter') {
													e.preventDefault();
													commitRename();
												} else if (e.key === 'Escape') {
													renaming = false;
												}
											}}
											onfocus={(e) => (e.currentTarget as HTMLInputElement).select()}
											aria-label="Folder name"
											data-testid="folder-rename-input"
											class="h-8 w-64 text-sm font-medium"
										/>
									{:else}
										<h2 class="truncate text-lg font-semibold">{selected.display_name}</h2>
									{/if}
									<div class="mt-1 flex items-center gap-2 text-sm text-muted-foreground">
										<span class="font-mono text-xs">{selected.path}</span>
									</div>
								</div>
								<div class="flex shrink-0 gap-1">
									<Button variant="outline" size="sm" onclick={startRename} data-testid="btn-rename-folder">
										<Pencil class="size-3.5" /> Rename
									</Button>
									<Button variant="outline" size="sm" onclick={startMove} data-testid="btn-move-folder">
										<FolderInput class="size-3.5" /> Move
									</Button>
									<Button
										variant="outline"
										size="sm"
										class="text-destructive hover:text-destructive"
										onclick={handleDelete}
										data-testid="btn-delete-folder"
									>
										<Trash2 class="size-3.5" />
									</Button>
								</div>
							</div>

							{#if moving}
								<div
									class="flex items-center gap-2 rounded-md border border-border bg-muted/40 px-3 py-2"
									data-testid="folder-move-panel"
								>
									<span class="text-sm text-muted-foreground">Move under</span>
									<Select.Root
										type="single"
										value={moveParentId ?? ROOT}
										onValueChange={(v) => (moveParentId = v === ROOT ? null : (v ?? null))}
									>
										<Select.Trigger class="h-8 flex-1 text-sm" data-testid="select-move-parent">
											<span class="truncate">{moveParentLabel}</span>
										</Select.Trigger>
										<Select.Content>
											<Select.Item value={ROOT} label="Workspace root" />
											{#each moveTargets(selected.id) as t (t.id)}
												<Select.Item value={t.id} label={t.path} />
											{/each}
										</Select.Content>
									</Select.Root>
									<Button size="sm" disabled={moveBusy} onclick={commitMove} data-testid="btn-confirm-move-folder">
										{moveBusy ? 'Moving…' : 'Move'}
									</Button>
									<Button size="sm" variant="ghost" onclick={() => (moving = false)}>Cancel</Button>
								</div>
							{/if}

							<!-- Per-folder OpenAPI bundle -->
							<div class="rounded-md border border-border bg-background/50 p-3">
								<div class="mb-2 flex items-center gap-2 text-sm font-medium">
									<BookOpen class="size-4 text-muted-foreground" />
									OpenAPI bundle
								</div>
								<div class="flex items-center gap-2">
									<code class="min-w-0 flex-1 truncate rounded bg-muted px-2 py-1 font-mono text-xs">
										{bundleUrl(selected)}
									</code>
									<Button
										variant="ghost"
										size="sm"
										title="Copy OpenAPI bundle URL"
										onclick={() => copyBundleUrl(selected!)}
										data-testid="btn-copy-bundle"
									>
										<Copy class="size-3.5" />
									</Button>
									<a
										href={bundleUrl(selected)}
										target="_blank"
										rel="noopener"
										class="inline-flex h-8 items-center justify-center rounded-md px-2 text-muted-foreground hover:bg-accent hover:text-foreground"
										title="Open OpenAPI bundle in new tab"
										data-testid="link-open-bundle"
									>
										<ExternalLink class="size-3.5" />
									</a>
								</div>
							</div>
						</div>
					{/if}
				</section>
			</div>
		{/if}
	{/if}
</div>
