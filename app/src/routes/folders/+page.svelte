<script lang="ts">
	import Plus from '@lucide/svelte/icons/plus';
	import Copy from '@lucide/svelte/icons/copy';
	import ExternalLink from '@lucide/svelte/icons/external-link';
	import Pencil from '@lucide/svelte/icons/pencil';
	import FolderInput from '@lucide/svelte/icons/folder-input';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import BookOpen from '@lucide/svelte/icons/book-open';
	import Share2 from '@lucide/svelte/icons/share-2';
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Select from '$lib/components/ui/select';
	import * as Tabs from '$lib/components/ui/tabs';
	import { PageShell, PageHeader, SideRail } from '$lib/components/shell';
	import { workspaces } from '$lib/workspaces/store.svelte';
	import {
		listFolders,
		createFolder,
		updateFolder,
		deleteFolder,
		forkFolder,
		type Folder
	} from '$lib/api/client';
	import { toast } from 'svelte-sonner';
	import GitFork from '@lucide/svelte/icons/git-fork';
	import FolderTreeRailSection from '$lib/components/folders/FolderTreeRailSection.svelte';
	import FolderApiContract from '$lib/components/folders/FolderApiContract.svelte';
	import FolderTemplatesPanel from '$lib/components/folders/FolderTemplatesPanel.svelte';
	import FolderSettingsPanel from '$lib/components/folders/FolderSettingsPanel.svelte';
	import ResourceList from '$lib/components/resources/ResourceList.svelte';
	import AssetList from '$lib/components/assets/AssetList.svelte';
	import FolderPagesPanel from '$lib/components/folders/FolderPagesPanel.svelte';
	import ShareDialog from '$lib/components/iam/ShareDialog.svelte';
	import AuthorshipChips from '$lib/components/iam/AuthorshipChips.svelte';
	import { roleAtLeast } from '$lib/api/iam';
	import { createListState } from '$lib/stores/remote.svelte';

	// Top-level folders are scoped to the active workspace — same implicit
	// scoping every other top-level page (Templates, Instances, …) uses.
	const workspaceId = $derived(workspaces.active?.id ?? '');

	const folderList = createListState((wsId: string) => listFolders(wsId), {
		errorFallback: 'Failed to load folders'
	});
	const folders = $derived(folderList.items);

	// Selection is URL-addressable (`/folders?folder=<id>`) so a folder's
	// contract can be deep-linked / shared without a separate detail route.
	let selectedId = $state<string | null>(page.url.searchParams.get('folder'));

	const selected = $derived(folders.find((f) => f.id === selectedId) ?? null);

	// New-folder form (collapsible, lives in the tree pane). Parent defaults
	// to whatever folder is selected in the tree.
	let createOpen = $state(false);
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

	// Per-object access (Phase 5). Edit affordances follow the caller's effective
	// role on the selected folder; the Share dialog needs object-Admin.
	let shareOpen = $state(false);
	const canEdit = $derived(roleAtLeast(selected?.my_effective_role, 'editor'));
	const canShare = $derived(roleAtLeast(selected?.my_effective_role, 'admin'));
	// Read-only here = you can't edit this folder (typically because you're
	// browsing the demos workspace as a non-member). The way to make its contents
	// your own — runnable in your tenant — is to fork the folder in.
	const isReadOnly = $derived(!!selected && !canEdit);

	let forking = $state(false);
	async function handleFork() {
		if (!selected || forking) return;
		forking = true;
		try {
			const res = await forkFolder(selected.id);
			const dest =
				workspaces.workspaces.find((w) => w.id === res.workspace_id)?.display_name ??
				'your workspace';
			const n = res.templates;
			toast.success(`Forked into ${dest} (${n} template${n === 1 ? '' : 's'})`);
			// The copy lands in a workspace you own (which may differ from the
			// demos workspace you're browsing), so don't move the demos selection.
			if (res.workspace_id === workspaceId) {
				await folderList.refetch();
				selectedId = res.folder_id;
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : 'Failed to fork folder');
		} finally {
			forking = false;
		}
	}

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

	$effect(() => {
		workspaces.load();
		if (workspaceId) void folderList.load(workspaceId);
	});

	// Keep the new-folder parent in lockstep with the tree selection.
	$effect(() => {
		newParentId = selectedId;
	});

	function selectFolder(id: string | null) {
		selectedId = id;
		renaming = false;
		moving = false;
		goto(id ? `/folders?folder=${id}` : '/folders', {
			replaceState: true,
			keepFocus: true,
			noScroll: true
		});
	}

	// Inline "new subfolder" affordance from a tree row: select the folder (which
	// mirrors into the create form's parent) and focus the slug input.
	function startSubfolder(parent: Folder) {
		selectFolder(parent.id);
		newSlug = '';
		newName = '';
		createOpen = true;
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
			folderList.items = [...folders, f];
			newSlug = '';
			newName = '';
			createOpen = false;
			selectFolder(f.id);
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
		folderList.items = folders.map((x) => (x.id === f.id ? { ...x, display_name: next } : x)); // optimistic
		try {
			await updateFolder(f.id, { display_name: next });
		} catch (e) {
			folderList.items = folders.map((x) => (x.id === f.id ? { ...x, display_name: prev } : x));
			folderList.error = e instanceof Error ? e.message : 'Rename failed';
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
		folderList.error = null;
		try {
			await updateFolder(f.id, { parent_id: moveParentId });
			moving = false;
			await folderList.refetch(); // materialized paths shift across the subtree — reload.
		} catch (e) {
			folderList.error = e instanceof Error ? e.message : 'Move failed';
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
			selectFolder(f.parent_id ?? null);
			await folderList.refetch();
		} catch (e) {
			folderList.error = e instanceof Error ? e.message : 'Failed to delete folder';
		}
	}

	// Folder edits from the Settings tab (slug changes re-root subtree paths).
	function handleUpdated(next: Folder) {
		folderList.items = folders.map((x) => (x.id === next.id ? next : x));
		void folderList.refetch();
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

<!-- Sidebar-page anatomy via the shell (same as /templates): pinned band
     flush left above the rail, SideRail tree, scrolling detail body. -->
<PageShell width="full" testid="folders-index">
	{#snippet band()}
		<PageHeader title="Folders">
			<p class="mt-1 text-sm text-muted-foreground">
				Organize templates into a hierarchy. Each folder exposes its own OpenAPI bundle
				with a runnable contract for every published template in its subtree, plus a
				dedicated endpoint per Manual/Webhook trigger.{#if workspaces.active}
					Workspace: <span class="font-medium">{workspaces.active.display_name}</span>.{/if}
			</p>
		</PageHeader>
	{/snippet}

	{#snippet sidebar()}
		<SideRail testid="folders-tree-panel">
			<div class="space-y-6 p-4">
				<FolderTreeRailSection {folders} {selectedId} onSelect={selectFolder} {actions}>
					{#snippet headerAction()}
						<Button
							variant="ghost"
							size="sm"
							class="size-7 p-0 text-muted-foreground"
							title="New folder"
							aria-label="New folder"
							onclick={() => (createOpen = !createOpen)}
							data-testid="btn-new-folder"
						>
							<Plus class="size-4" />
						</Button>
					{/snippet}
					{#snippet extra()}
						{#if createOpen}
							<form
								onsubmit={handleCreate}
								class="mb-3 space-y-2 rounded-md border border-border/60 p-2"
							>
								<Input
									placeholder="slug"
									bind:value={newSlug}
									data-testid="input-new-folder-slug"
								/>
								<Input
									placeholder="Display name"
									bind:value={newName}
									data-testid="input-new-folder-name"
								/>
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
								{#if createError}
									<div class="text-sm text-destructive">{createError}</div>
								{/if}
								<div class="flex gap-2">
									<Button type="submit" size="sm" disabled={creating} data-testid="btn-create-folder">
										<Plus class="size-3.5" />
										{creating ? 'Creating…' : 'Create'}
									</Button>
									<Button
										type="button"
										size="sm"
										variant="ghost"
										onclick={() => {
											createOpen = false;
											createError = null;
										}}
									>
										Cancel
									</Button>
								</div>
							</form>
						{/if}
					{/snippet}
				</FolderTreeRailSection>
			</div>
		</SideRail>
	{/snippet}

	{#if !workspaceId}
		<div class="rounded-lg border border-dashed border-border p-6 text-sm text-muted-foreground">
			No active workspace. Pick one from the workspace switcher first.
		</div>
	{:else if folderList.loading}
		<p class="text-sm text-muted-foreground">Loading…</p>
	{:else}
		<div data-testid="folder-detail">
			{#if folderList.error}
				<div class="mb-4 rounded-md border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive">
					{folderList.error}
				</div>
			{/if}
			{#if !selected}
				<div class="py-16 text-center text-sm text-muted-foreground">
					Select a folder from the tree to see its API contract, or create one.
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
									<AuthorshipChips
										class="mt-1.5"
										createdBy={selected.created_by}
										createdAt={selected.created_at}
										updatedBy={selected.updated_by}
										updatedAt={selected.updated_at}
									/>
								</div>
								<div class="flex shrink-0 gap-1">
									{#if isReadOnly}
										<Button
											variant="outline"
											size="sm"
											onclick={handleFork}
											disabled={forking}
											data-testid="btn-fork-folder"
										>
											<GitFork class="size-3.5" /> {forking ? 'Forking…' : 'Fork to workspace'}
										</Button>
									{/if}
									{#if canShare}
										<Button
											variant="outline"
											size="sm"
											onclick={() => (shareOpen = true)}
											data-testid="btn-share-folder"
										>
											<Share2 class="size-3.5" /> Share
										</Button>
									{/if}
									{#if canEdit}
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
									{/if}
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

							<!-- Contract + management tabs. Keyed by folder so per-folder
							     state (active tab, drafts) resets on selection change. -->
							{#key selected.id}
								<Tabs.Root value="endpoints">
									<!-- Full-width rule under the tab row; the -mb-px pulls the
									     triggers' 2px active underline down to overlap it
									     (same anatomy as the PageShell band tabs). -->
									<div class="border-b border-border">
										<Tabs.List variant="underline" class="-mb-px">
											<Tabs.Trigger variant="underline" value="endpoints" data-testid="tab-endpoints">
												Endpoints
											</Tabs.Trigger>
											<Tabs.Trigger variant="underline" value="templates" data-testid="tab-templates">
												Templates
											</Tabs.Trigger>
											<Tabs.Trigger variant="underline" value="resources" data-testid="tab-resources">
												Resources
											</Tabs.Trigger>
											<Tabs.Trigger variant="underline" value="assets" data-testid="tab-assets">
												Assets
											</Tabs.Trigger>
											<Tabs.Trigger variant="underline" value="pages" data-testid="tab-pages">
												Pages
											</Tabs.Trigger>
											<Tabs.Trigger variant="underline" value="settings" data-testid="tab-settings">
												Settings
											</Tabs.Trigger>
										</Tabs.List>
									</div>
									<Tabs.Content value="endpoints" class="pt-4">
										<FolderApiContract workspaceId={selected.workspace_id} folderId={selected.id} />
									</Tabs.Content>
									<Tabs.Content value="templates" class="pt-4">
										<FolderTemplatesPanel folderId={selected.id} />
									</Tabs.Content>
									<Tabs.Content value="resources" class="pt-4">
										<ResourceList workspace_id={selected.workspace_id} folderId={selected.id} />
									</Tabs.Content>
									<Tabs.Content value="assets" class="pt-4">
										<AssetList scope={{ kind: 'folder', id: selected.id }} />
									</Tabs.Content>
									<Tabs.Content value="pages" class="pt-4">
										<FolderPagesPanel
											folderId={selected.id}
											editableRole={selected.my_effective_role}
										/>
									</Tabs.Content>
									<Tabs.Content value="settings" class="pt-4">
										<FolderSettingsPanel folder={selected} onUpdated={handleUpdated} />
									</Tabs.Content>
								</Tabs.Root>
							{/key}
				</div>
			{/if}
		</div>
	{/if}
</PageShell>

{#if selected}
	<ShareDialog
		bind:open={shareOpen}
		objectType="folder"
		objectId={selected.id}
		objectName={selected.display_name}
		myEffectiveRole={selected.my_effective_role}
		onChanged={() => void folderList.refetch()}
	/>
{/if}
