<script lang="ts">
	import * as Dialog from '$lib/components/ui/dialog';
	import { Button } from '$lib/components/ui/button';
	import { Input } from '$lib/components/ui/input';
	import * as Select from '$lib/components/ui/select';
	import FolderIcon from '@lucide/svelte/icons/folder';
	import Plus from '@lucide/svelte/icons/plus';
	import Pencil from '@lucide/svelte/icons/pencil';
	import FolderInput from '@lucide/svelte/icons/folder-input';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import {
		listFolders,
		createFolder,
		updateFolder,
		deleteFolder,
		type Folder
	} from '$lib/api/client';
	import { workspaces } from '$lib/workspaces/store.svelte';

	interface Props {
		open: boolean;
		/** Fired after any create/rename/move/delete so the parent can refresh filters. */
		onChanged?: () => void;
	}

	let { open = $bindable(), onChanged }: Props = $props();

	let folders = $state<Folder[]>([]);
	let loading = $state(false);
	let error = $state<string | null>(null);

	let newName = $state('');
	let newParentId = $state<string | null>(null);
	let creating = $state(false);

	let renamingId = $state<string | null>(null);
	let renameDraft = $state('');
	let renameRef = $state<HTMLInputElement | null>(null);

	// Move dialog state — picks a new parent (or workspace root) for one folder.
	let movingId = $state<string | null>(null);
	let moveParentId = $state<string | null>(null);
	let moving = $state(false);

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
				.slice(0, 48) || 'folder'
		);
	}

	// Folders sorted by materialized path so the flat list reads as a tree.
	const sorted = $derived([...folders].sort((a, b) => a.path.localeCompare(b.path)));

	// Depth from the materialized path (`/research/q3` → depth 1). Used to indent.
	function depthOf(f: Folder): number {
		const segs = f.path.split('/').filter(Boolean);
		return Math.max(0, segs.length - 1);
	}

	// All folders except `excludeId` and its descendants — valid move targets.
	function moveTargets(excludeId: string | null): Folder[] {
		if (!excludeId) return sorted;
		const self = folders.find((f) => f.id === excludeId);
		const prefix = self ? `${self.path}/` : '';
		return sorted.filter(
			(f) => f.id !== excludeId && !(self && (f.path === self.path || f.path.startsWith(prefix)))
		);
	}

	async function loadFolders() {
		const ws = workspaces.active?.id;
		if (!ws) {
			folders = [];
			return;
		}
		loading = true;
		error = null;
		try {
			folders = await listFolders(ws);
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load folders';
			folders = [];
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (open) {
			error = null;
			renamingId = null;
			movingId = null;
			newName = '';
			newParentId = null;
			loadFolders();
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
			const f = await createFolder(ws, {
				slug: slugify(name),
				display_name: name,
				description: '',
				parent_id: newParentId ?? undefined
			});
			folders = [...folders, f];
			newName = '';
			newParentId = null;
			onChanged?.();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to create folder';
		} finally {
			creating = false;
		}
	}

	function startRename(f: Folder) {
		movingId = null;
		renamingId = f.id;
		renameDraft = f.display_name;
	}

	async function commitRename() {
		const id = renamingId;
		if (!id) return;
		renamingId = null;
		const f = folders.find((x) => x.id === id);
		if (!f) return;
		const next = renameDraft.trim();
		if (!next || next === f.display_name) return;
		const prev = f.display_name;
		folders = folders.map((x) => (x.id === id ? { ...x, display_name: next } : x)); // optimistic
		try {
			await updateFolder(id, { display_name: next });
			onChanged?.();
		} catch (e) {
			folders = folders.map((x) => (x.id === id ? { ...x, display_name: prev } : x));
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

	function startMove(f: Folder) {
		renamingId = null;
		movingId = f.id;
		moveParentId = f.parent_id ?? null;
	}

	async function commitMove() {
		const id = movingId;
		if (!id || moving) return;
		moving = true;
		error = null;
		try {
			// `parent_id = null` re-roots to the workspace top level.
			await updateFolder(id, { parent_id: moveParentId });
			movingId = null;
			await loadFolders(); // paths shift across the subtree — reload authoritative.
			onChanged?.();
		} catch (e) {
			error = e instanceof Error ? e.message : 'Move failed';
		} finally {
			moving = false;
		}
	}

	async function remove(f: Folder) {
		if (
			!confirm(
				`Delete folder “${f.display_name}”? Templates are unaffected — only the grouping is removed. Any child folders are reparented to this folder's parent.`
			)
		)
			return;
		const prev = folders;
		folders = folders.filter((x) => x.id !== f.id); // optimistic
		error = null;
		try {
			await deleteFolder(f.id);
			await loadFolders(); // children reparent server-side — reload to reflect.
			onChanged?.();
		} catch (e) {
			folders = prev;
			error = e instanceof Error ? e.message : 'Delete failed';
		}
	}

	function onNewKeydown(e: KeyboardEvent) {
		if (e.key === 'Enter') {
			e.preventDefault();
			create();
		}
	}

	// Select value <-> nullable parent id. Root uses a sentinel (bits-ui Select
	// reserves the empty string).
	const ROOT = '__root__';
	const newParentLabel = $derived(
		newParentId ? (folders.find((f) => f.id === newParentId)?.path ?? '—') : 'Workspace root'
	);
	const moveParentLabel = $derived(
		moveParentId ? (folders.find((f) => f.id === moveParentId)?.path ?? '—') : 'Workspace root'
	);
</script>

<Dialog.Root bind:open>
	<Dialog.Content class="sm:max-w-lg" data-testid="manage-folders-dialog">
		<Dialog.Header>
			<Dialog.Title>Manage folders</Dialog.Title>
			<Dialog.Description>
				Folders group templates within {workspaces.active?.display_name ?? 'this workspace'}.
				They are labels, not a permission boundary.
			</Dialog.Description>
		</Dialog.Header>

		<div class="space-y-4">
			<!-- Existing folders -->
			{#if loading}
				<div class="py-6 text-center text-sm text-muted-foreground">Loading folders…</div>
			{:else if folders.length === 0}
				<div class="rounded-md border border-dashed border-border py-6 text-center text-sm text-muted-foreground">
					No folders yet
				</div>
			{:else}
				<ul class="divide-y divide-border rounded-md border border-border" data-testid="manage-folders-list">
					{#each sorted as f (f.id)}
						<li class="px-3 py-2" data-testid={`manage-folder-${f.slug}`}>
							<div class="flex items-center gap-2" style={`padding-left: ${depthOf(f) * 14}px`}>
								<FolderIcon class="size-4 shrink-0 text-muted-foreground" />
								{#if renamingId === f.id}
									<Input
										bind:ref={renameRef}
										bind:value={renameDraft}
										onkeydown={onRenameKeydown}
										onblur={commitRename}
										onfocus={(e) => (e.currentTarget as HTMLInputElement).select()}
										aria-label="Folder name"
										data-testid={`manage-folder-rename-input-${f.slug}`}
										class="h-7 flex-1 text-sm"
									/>
								{:else}
									<span class="flex-1 truncate text-sm text-foreground">{f.display_name}</span>
									<span class="shrink-0 font-mono text-xs text-muted-foreground/60">{f.slug}</span>
								{/if}
								<Button
									variant="ghost"
									size="sm"
									class="size-7 p-0"
									aria-label={`Rename ${f.display_name}`}
									onclick={() => startRename(f)}
									data-testid={`manage-folder-rename-${f.slug}`}
								>
									<Pencil class="size-3.5" />
								</Button>
								<Button
									variant="ghost"
									size="sm"
									class="size-7 p-0"
									aria-label={`Move ${f.display_name}`}
									onclick={() => startMove(f)}
									data-testid={`manage-folder-move-${f.slug}`}
								>
									<FolderInput class="size-3.5" />
								</Button>
								<Button
									variant="ghost"
									size="sm"
									class="size-7 p-0 text-muted-foreground hover:text-destructive"
									aria-label={`Delete ${f.display_name}`}
									onclick={() => remove(f)}
									data-testid={`manage-folder-delete-${f.slug}`}
								>
									<Trash2 class="size-3.5" />
								</Button>
							</div>

							{#if movingId === f.id}
								<div
									class="mt-2 flex items-center gap-2 rounded-md border border-border bg-muted/40 px-2 py-2"
									data-testid={`manage-folder-move-panel-${f.slug}`}
								>
									<span class="text-xs text-muted-foreground">Move under</span>
									<Select.Root
										type="single"
										value={moveParentId ?? ROOT}
										onValueChange={(v) => (moveParentId = v === ROOT ? null : (v ?? null))}
									>
										<Select.Trigger class="h-8 flex-1 text-sm" data-testid={`move-parent-trigger-${f.slug}`}>
											<span class="truncate">{moveParentLabel}</span>
										</Select.Trigger>
										<Select.Content>
											<Select.Item value={ROOT} label="Workspace root" />
											{#each moveTargets(f.id) as t (t.id)}
												<Select.Item value={t.id} label={t.path} />
											{/each}
										</Select.Content>
									</Select.Root>
									<Button size="sm" disabled={moving} onclick={commitMove} data-testid={`move-confirm-${f.slug}`}>
										{moving ? 'Moving…' : 'Move'}
									</Button>
									<Button size="sm" variant="ghost" onclick={() => (movingId = null)}>Cancel</Button>
								</div>
							{/if}
						</li>
					{/each}
				</ul>
			{/if}

			<!-- Add folder -->
			<div class="space-y-2 rounded-md border border-border p-3">
				<div class="flex items-center gap-2">
					<Input
						placeholder="New folder name…"
						bind:value={newName}
						onkeydown={onNewKeydown}
						data-testid="manage-folder-new-input"
						class="flex-1"
					/>
					<Button onclick={create} disabled={!newName.trim() || creating} data-testid="manage-folder-create">
						<Plus class="size-4" />
						{creating ? 'Adding…' : 'Add'}
					</Button>
				</div>
				<div class="flex items-center gap-2">
					<span class="text-xs text-muted-foreground">Parent</span>
					<Select.Root
						type="single"
						value={newParentId ?? ROOT}
						onValueChange={(v) => (newParentId = v === ROOT ? null : (v ?? null))}
					>
						<Select.Trigger class="h-8 flex-1 text-sm" data-testid="new-folder-parent-trigger">
							<span class="truncate">{newParentLabel}</span>
						</Select.Trigger>
						<Select.Content>
							<Select.Item value={ROOT} label="Workspace root" />
							{#each sorted as t (t.id)}
								<Select.Item value={t.id} label={t.path} />
							{/each}
						</Select.Content>
					</Select.Root>
				</div>
			</div>

			{#if error}
				<div class="text-sm text-destructive" data-testid="manage-folders-error">{error}</div>
			{/if}
		</div>
	</Dialog.Content>
</Dialog.Root>
