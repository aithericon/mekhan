<script lang="ts">
	// Free (folder-homed) pages: collaborative rich-text docs that live in a
	// folder rather than being attached to a template/instance. Body of the
	// Pages tab in the folders browser's detail pane. Optimistic create / inline
	// rename / confirm-guarded delete, all gated on the caller's effective role
	// on the selected folder (mirrors the inline CRUD in routes/folders/+page).
	import Plus from '@lucide/svelte/icons/plus';
	import Pencil from '@lucide/svelte/icons/pencil';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import FileText from '@lucide/svelte/icons/file-text';
	import { goto } from '$app/navigation';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import { roleAtLeast } from '$lib/api/iam';
	import { createListState } from '$lib/stores/remote.svelte';
	import {
		listFolderPages,
		createPage,
		updatePage,
		deletePage,
		type Page
	} from '$lib/api/client';
	import {
		createFolderPage,
		renameFolderPage,
		deleteFolderPage,
		type PageListOps
	} from './folder-pages-logic';

	type Props = {
		folderId: string;
		/** The caller's effective role on the host folder; create/rename/delete
		 *  require >= editor. Panel is ACL-driven by this prop, not self-resolved. */
		editableRole?: string | null;
	};
	let { folderId, editableRole = null }: Props = $props();

	const canEdit = $derived(roleAtLeast(editableRole, 'editor'));

	const pageList = createListState((id: string) => listFolderPages(id), {
		errorFallback: 'Failed to load pages'
	});
	const pages = $derived(pageList.items);

	// `pageList` exposes `items` (get/set) — the shape the optimistic helpers want.
	const ops: PageListOps = { createPage, updatePage, deletePage };

	$effect(() => {
		if (folderId) void pageList.load(folderId);
	});

	// New-page inline form.
	let createOpen = $state(false);
	let newTitle = $state('');
	let creating = $state(false);

	// Inline rename of one row.
	let renamingId = $state<string | null>(null);
	let renameDraft = $state('');

	async function handleCreate(e: Event) {
		e.preventDefault();
		if (!canEdit || creating) return;
		creating = true;
		pageList.error = null;
		try {
			const created = await createFolderPage(ops, pageList, folderId, newTitle);
			newTitle = '';
			createOpen = false;
			// Jump straight into the new doc — that's the point of creating it.
			void goto(`/pages/${created.id}`);
		} catch (e) {
			pageList.error = e instanceof Error ? e.message : 'Failed to create page';
		} finally {
			creating = false;
		}
	}

	function startRename(p: Page) {
		if (!canEdit) return;
		renamingId = p.id;
		renameDraft = p.title;
	}

	async function commitRename(id: string) {
		const next = renameDraft;
		renamingId = null;
		try {
			await renameFolderPage(ops, pageList, id, next);
		} catch (e) {
			pageList.error = e instanceof Error ? e.message : 'Rename failed';
		}
	}

	async function handleDelete(p: Page) {
		if (!canEdit) return;
		if (!confirm(`Delete page “${p.title}”? Its content is permanently removed.`)) return;
		try {
			await deleteFolderPage(ops, pageList, p.id);
		} catch (e) {
			pageList.error = e instanceof Error ? e.message : 'Failed to delete page';
		}
	}
</script>

<div class="space-y-4" data-testid="folder-pages-panel">
	<div class="flex items-start justify-between gap-2">
		<p class="text-sm text-muted-foreground">
			Rich-text pages homed in this folder — notes, runbooks, write-ups. Edited collaboratively.
		</p>
		{#if canEdit}
			<Button
				variant="outline"
				size="sm"
				class="shrink-0"
				onclick={() => {
					createOpen = !createOpen;
					newTitle = '';
				}}
				data-testid="btn-new-page"
			>
				<Plus class="size-3.5" /> New page
			</Button>
		{/if}
	</div>

	{#if pageList.error}
		<div
			class="rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
		>
			{pageList.error}
		</div>
	{/if}

	{#if createOpen && canEdit}
		<form
			onsubmit={handleCreate}
			class="flex items-center gap-2 rounded-md border border-border/60 p-2"
		>
			<Input
				placeholder="Page title"
				bind:value={newTitle}
				data-testid="input-new-page-title"
				autofocus
			/>
			<Button type="submit" size="sm" disabled={creating} data-testid="btn-create-page">
				<Plus class="size-3.5" />
				{creating ? 'Creating…' : 'Create'}
			</Button>
			<Button
				type="button"
				size="sm"
				variant="ghost"
				onclick={() => {
					createOpen = false;
					newTitle = '';
				}}
			>
				Cancel
			</Button>
		</form>
	{/if}

	{#if pageList.loading}
		<p class="text-sm text-muted-foreground">Loading…</p>
	{:else if pages.length === 0}
		<p class="text-sm text-muted-foreground">No pages in this folder yet.</p>
	{:else}
		<ul class="space-y-2">
			{#each pages as p (p.id)}
				<li
					class="flex items-center justify-between gap-2 rounded-md border border-border bg-card/50 px-3 py-2 text-sm"
					data-testid={`page-row-${p.id}`}
				>
					{#if renamingId === p.id}
						<Input
							bind:value={renameDraft}
							onblur={() => commitRename(p.id)}
							onkeydown={(e) => {
								if (e.key === 'Enter') {
									e.preventDefault();
									commitRename(p.id);
								} else if (e.key === 'Escape') {
									renamingId = null;
								}
							}}
							onfocus={(e) => (e.currentTarget as HTMLInputElement).select()}
							aria-label="Page title"
							data-testid={`page-rename-input-${p.id}`}
							class="h-8 flex-1 text-sm"
						/>
					{:else}
						<a
							href={`/pages/${p.id}`}
							class="flex min-w-0 flex-1 items-center gap-2 hover:underline"
						>
							<FileText class="size-4 shrink-0 text-muted-foreground" />
							<span class="truncate font-medium">{p.title}</span>
						</a>
					{/if}

					{#if canEdit && renamingId !== p.id}
						<div class="flex shrink-0 gap-1">
							<Button
								variant="ghost"
								size="sm"
								class="size-7 p-0"
								title="Rename page"
								aria-label="Rename page"
								onclick={() => startRename(p)}
								data-testid={`btn-rename-page-${p.id}`}
							>
								<Pencil class="size-3.5" />
							</Button>
							<Button
								variant="ghost"
								size="sm"
								class="size-7 p-0 text-destructive hover:text-destructive"
								title="Delete page"
								aria-label="Delete page"
								onclick={() => handleDelete(p)}
								data-testid={`btn-delete-page-${p.id}`}
							>
								<Trash2 class="size-3.5" />
							</Button>
						</div>
					{/if}
				</li>
			{/each}
		</ul>
	{/if}
</div>
