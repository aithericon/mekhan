<script lang="ts">
	import Tag from '@lucide/svelte/icons/tag';
	import Settings2 from '@lucide/svelte/icons/settings-2';
	import { Button } from '$lib/components/ui/button';
	import { listFolders, listWorkspaceTags, type Folder } from '$lib/api/client';
	import { workspaces } from '$lib/workspaces/store.svelte';
	import FolderTreeRailSection from '$lib/components/folders/FolderTreeRailSection.svelte';
	import { SideRail } from '$lib/components/shell';

	interface Props {
		folderId: string | null;
		recursive: boolean;
		tag: string | null;
		onChange: (next: { folderId: string | null; recursive: boolean; tag: string | null }) => void;
	}

	let { folderId, recursive, tag, onChange }: Props = $props();

	let folders = $state<Folder[]>([]);
	let tags = $state<string[]>([]);
	let loading = $state(false);

	async function loadFilters(workspaceId: string) {
		loading = true;
		try {
			folders = await listFolders(workspaceId);
		} catch {
			folders = [];
		}
		try {
			tags = await listWorkspaceTags(workspaceId);
		} catch {
			tags = [];
		}
		loading = false;
	}

	$effect(() => {
		const ws = workspaces.active?.id;
		if (ws) loadFilters(ws);
	});

	function selectFolder(id: string | null) {
		onChange({ folderId: id, recursive, tag });
	}

	function toggleRecursive(next: boolean) {
		onChange({ folderId, recursive: next, tag });
	}

	function selectTag(t: string | null) {
		onChange({ folderId, recursive, tag: t });
	}
</script>

<SideRail testid="templates-filters-sidebar">
	<div class="space-y-6 p-4">
		<FolderTreeRailSection {folders} selectedId={folderId} onSelect={selectFolder}>
			{#snippet headerAction()}
				<!-- Folder management lives on /folders (the browser); deep-link
				     the current filter selection into it. -->
				<Button
					variant="ghost"
					size="sm"
					class="size-7 p-0 text-muted-foreground"
					title="Manage folders"
					aria-label="Manage folders"
					href={folderId ? `/folders?folder=${folderId}` : '/folders'}
					data-testid="btn-manage-folders"
				>
					<Settings2 class="size-4" />
				</Button>
			{/snippet}

			<label
				class="mt-2 flex items-center gap-1.5 px-2 text-sm text-muted-foreground"
				title="Include templates in nested subfolders of the selected folder"
			>
				<input
					type="checkbox"
					class="size-3.5"
					checked={recursive}
					disabled={folderId === null}
					onchange={(e) => toggleRecursive((e.currentTarget as HTMLInputElement).checked)}
					data-testid="filter-folder-recursive"
				/>
				Include subfolders
			</label>
		</FolderTreeRailSection>

		{#if tags.length > 0}
			<section>
				<div class="mb-2 flex items-center gap-2 text-sm font-medium text-foreground">
					<Tag class="size-4 text-muted-foreground" />
					Tags
				</div>
				<div class="flex flex-wrap gap-1" data-testid="filter-tags">
					{#each tags as t (t)}
						<button
							type="button"
							class="rounded border px-2 py-0.5 text-sm transition-colors {tag === t ? 'border-foreground bg-foreground text-background' : 'border-border text-muted-foreground hover:bg-accent'}"
							onclick={() => selectTag(tag === t ? null : t)}
							data-testid={`filter-tag-${t}`}
						>
							{t}
						</button>
					{/each}
				</div>
			</section>
		{/if}

		{#if loading}
			<div class="text-sm text-muted-foreground">Loading filters…</div>
		{/if}
	</div>
</SideRail>
