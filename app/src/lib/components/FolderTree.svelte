<script lang="ts">
	import Folder from '@lucide/svelte/icons/folder';
	import FolderOpen from '@lucide/svelte/icons/folder-open';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import Layers from '@lucide/svelte/icons/layers';
	import type { Folder as FolderType } from '$lib/api/client';

	interface Props {
		/** Flat list of all folders in the workspace. */
		folders: FolderType[];
		/** Currently selected folder id; `null` = the "All templates" root. */
		selectedId: string | null;
		onSelect: (id: string | null) => void;
	}

	let { folders, selectedId, onSelect }: Props = $props();

	// parent_id -> direct children, ordered by display name. A `null`/absent
	// parent_id is a root-level folder (keyed under the empty string).
	const childrenByParent = $derived.by(() => {
		const map = new Map<string, FolderType[]>();
		for (const f of folders) {
			const key = f.parent_id ?? '';
			const arr = map.get(key) ?? [];
			arr.push(f);
			map.set(key, arr);
		}
		for (const arr of map.values()) {
			arr.sort((a, b) => a.display_name.localeCompare(b.display_name));
		}
		return map;
	});

	const roots = $derived(childrenByParent.get('') ?? []);

	// Expansion state, keyed by folder id. Folders default to expanded so the
	// whole tree is visible on first paint; toggling collapses a subtree.
	let collapsed = $state<Set<string>>(new Set());

	function toggle(id: string) {
		const next = new Set(collapsed);
		if (next.has(id)) next.delete(id);
		else next.add(id);
		collapsed = next;
	}
</script>

{#snippet node(f: FolderType, depth: number)}
	{@const kids = childrenByParent.get(f.id) ?? []}
	{@const hasKids = kids.length > 0}
	{@const isOpen = !collapsed.has(f.id)}
	{@const selected = selectedId === f.id}
	<li>
		<div
			class="group flex items-center gap-1 rounded pr-1 hover:bg-accent {selected
				? 'bg-accent text-foreground'
				: 'text-muted-foreground'}"
			style={`padding-left: ${depth * 12}px`}
		>
			{#if hasKids}
				<button
					type="button"
					class="flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground hover:text-foreground"
					onclick={() => toggle(f.id)}
					aria-label={isOpen ? 'Collapse' : 'Expand'}
					data-testid={`folder-toggle-${f.slug}`}
				>
					<ChevronRight class="size-3.5 transition-transform {isOpen ? 'rotate-90' : ''}" />
				</button>
			{:else}
				<span class="size-5 shrink-0"></span>
			{/if}
			<button
				type="button"
				class="flex min-w-0 flex-1 items-center gap-1.5 py-1 text-left text-sm {selected
					? 'font-medium'
					: ''}"
				onclick={() => onSelect(f.id)}
				data-testid={`filter-folder-${f.slug}`}
				title={f.description || f.path}
			>
				{#if isOpen && hasKids}
					<FolderOpen class="size-4 shrink-0 text-muted-foreground" />
				{:else}
					<Folder class="size-4 shrink-0 text-muted-foreground" />
				{/if}
				<span class="truncate">{f.display_name}</span>
			</button>
		</div>
		{#if hasKids && isOpen}
			<ul>
				{#each kids as child (child.id)}
					{@render node(child, depth + 1)}
				{/each}
			</ul>
		{/if}
	</li>
{/snippet}

<ul class="space-y-0.5" data-testid="folder-tree">
	<li>
		<button
			type="button"
			class="flex w-full items-center gap-1.5 rounded px-2 py-1 text-left text-sm hover:bg-accent {selectedId ===
			null
				? 'bg-accent font-medium text-foreground'
				: 'text-muted-foreground'}"
			onclick={() => onSelect(null)}
			data-testid="filter-folder-all"
		>
			<Layers class="size-4 shrink-0 text-muted-foreground" />
			All templates
		</button>
	</li>
	{#each roots as root (root.id)}
		{@render node(root, 0)}
	{:else}
		<li class="px-2 py-1 text-sm text-muted-foreground/60 italic">No folders yet</li>
	{/each}
</ul>
