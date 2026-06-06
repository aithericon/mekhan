<script lang="ts">
	import * as Command from '$lib/components/ui/command';
	import FolderIcon from '@lucide/svelte/icons/folder';
	import Layers from '@lucide/svelte/icons/layers';
	import Check from '@lucide/svelte/icons/check';
	import LoaderCircle from '@lucide/svelte/icons/loader-circle';
	import {
		listFolders,
		getTemplateFolder,
		setTemplateFolder,
		type Folder
	} from '$lib/api/client';
	import { workspaces } from '$lib/workspaces/store.svelte';

	interface Props {
		open: boolean;
		/** Version id — used to read + set the home folder. */
		templateId: string | null;
		templateName?: string;
		/** Fired after the home folder changes so the caller can refresh. */
		onMoved?: () => void;
	}

	let { open = $bindable(), templateId, templateName, onMoved }: Props = $props();

	let folders = $state<Folder[]>([]);
	// `null` = workspace root (no home folder).
	let currentFolderId = $state<string | null>(null);
	let loading = $state(false);
	let error = $state<string | null>(null);
	// Sentinel target for the in-flight set: the folder id being assigned, or
	// the empty string for "workspace root".
	let busyTarget = $state<string | null>(null);

	// Tree-sorted by materialized path.
	const sorted = $derived([...folders].sort((a, b) => a.path.localeCompare(b.path)));

	function depthOf(f: Folder): number {
		return Math.max(0, f.path.split('/').filter(Boolean).length - 1);
	}

	async function load() {
		const ws = workspaces.active?.id;
		if (!ws || !templateId) {
			folders = [];
			currentFolderId = null;
			return;
		}
		loading = true;
		error = null;
		try {
			const [all, home] = await Promise.all([
				listFolders(ws),
				getTemplateFolder(templateId)
			]);
			folders = all;
			currentFolderId = home?.id ?? null;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to load folders';
			folders = [];
			currentFolderId = null;
		} finally {
			loading = false;
		}
	}

	$effect(() => {
		if (open) load();
	});

	const ROOT = '';

	async function assign(target: string | null) {
		if (!templateId || busyTarget !== null) return;
		// No-op when picking the current home.
		if ((target ?? null) === currentFolderId) {
			open = false;
			return;
		}
		busyTarget = target ?? ROOT;
		error = null;
		try {
			await setTemplateFolder(templateId, { folder_id: target });
			currentFolderId = target ?? null;
			onMoved?.();
			open = false;
		} catch (e) {
			error = e instanceof Error ? e.message : 'Failed to move template';
		} finally {
			busyTarget = null;
		}
	}
</script>

<Command.Dialog
	bind:open
	title="Move to folder"
	description={`Set the home folder for ${templateName ?? 'this template'}.`}
	class="max-w-md"
>
	<Command.Input placeholder="Search folders…" data-testid="move-folder-search" />
	<Command.List>
		{#if loading}
			<div class="py-6 text-center text-sm text-muted-foreground">Loading folders…</div>
		{:else if !workspaces.active}
			<div class="py-6 text-center text-sm text-muted-foreground">No active workspace.</div>
		{:else}
			<Command.Empty>No folders found. Create one in Manage folders.</Command.Empty>
			<Command.Group heading="Home folder">
				<!-- Workspace root (no home folder). -->
				<Command.Item
					value="__root__"
					keywords={['workspace root', 'none', 'all templates']}
					onSelect={() => assign(null)}
					data-testid="move-folder-root"
				>
					<Layers class="size-4 text-muted-foreground" />
					<span class="flex-1 truncate">Workspace root</span>
					{#if busyTarget === ROOT}
						<LoaderCircle class="size-4 animate-spin text-muted-foreground" />
					{:else if currentFolderId === null}
						<span class="flex items-center gap-1 text-xs text-green-600" data-testid="move-folder-current-root">
							<Check class="size-4" />
							Current
						</span>
					{/if}
				</Command.Item>

				{#each sorted as f (f.id)}
					{@const current = currentFolderId === f.id}
					<Command.Item
						value={f.id}
						keywords={[f.display_name, f.slug, f.path]}
						onSelect={() => assign(f.id)}
						data-testid={`move-folder-${f.slug}`}
					>
						<span style={`width: ${depthOf(f) * 12}px`} class="shrink-0"></span>
						<FolderIcon class="size-4 text-muted-foreground" />
						<span class="flex-1 truncate">{f.display_name}</span>
						{#if busyTarget === f.id}
							<LoaderCircle class="size-4 animate-spin text-muted-foreground" />
						{:else if current}
							<span class="flex items-center gap-1 text-xs text-green-600" data-testid={`move-folder-current-${f.slug}`}>
								<Check class="size-4" />
								Current
							</span>
						{/if}
					</Command.Item>
				{/each}
			</Command.Group>
		{/if}
		{#if error}
			<div class="px-3 py-2 text-sm text-destructive" data-testid="move-folder-error">
				{error}
			</div>
		{/if}
	</Command.List>
</Command.Dialog>
