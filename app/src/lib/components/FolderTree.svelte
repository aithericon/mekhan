<script lang="ts">
	// Recursive folder tree for the template hierarchy. Built on the
	// shadcn-svelte-extras `TreeView` primitive (role="tree" container + the
	// guide-line aesthetic) and `Collapsible` (smooth, accessible expand/collapse
	// per node). The interaction model is deliberately a *split*: the chevron
	// toggles expansion, the label selects (filters / opens) — the extras
	// `TreeView.Folder` couples both onto one trigger, which is wrong for a
	// selectable filter/manager tree, so we compose the primitives directly.
	import Folder from '@lucide/svelte/icons/folder';
	import FolderOpen from '@lucide/svelte/icons/folder-open';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import Layers from '@lucide/svelte/icons/layers';
	import type { Snippet } from 'svelte';
	import * as TreeView from '$lib/components/ui/tree-view';
	import * as Collapsible from '$lib/components/ui/collapsible';
	import type { Folder as FolderType } from '$lib/api/client';

	interface Props {
		/** Flat list of all folders in the workspace. */
		folders: FolderType[];
		/** Currently selected folder id; `null` = the "All templates" root. */
		selectedId: string | null;
		onSelect: (id: string | null) => void;
		/** Optional per-folder actions, revealed on row hover (manager page). */
		actions?: Snippet<[FolderType]>;
		/** Label for the synthetic root row. */
		rootLabel?: string;
		/** Hide the synthetic "All templates" root row (e.g. pure managers). */
		showRoot?: boolean;
	}

	let {
		folders,
		selectedId,
		onSelect,
		actions,
		rootLabel = 'All templates',
		showRoot = true
	}: Props = $props();

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

	function setOpen(id: string, open: boolean) {
		const next = new Set(collapsed);
		if (open) next.delete(id);
		else next.add(id);
		collapsed = next;
	}
</script>

{#snippet node(f: FolderType, depth: number)}
	{@const kids = childrenByParent.get(f.id) ?? []}
	{@const hasKids = kids.length > 0}
	{@const isOpen = !collapsed.has(f.id)}
	{@const selected = selectedId === f.id}
	<Collapsible.Root open={isOpen} onOpenChange={(v) => setOpen(f.id, v)}>
		<div
			class="group flex items-center gap-1 rounded pr-1 hover:bg-accent {selected
				? 'bg-accent text-foreground'
				: 'text-muted-foreground'}"
			role="treeitem"
			aria-selected={selected}
			aria-expanded={hasKids ? isOpen : undefined}
		>
			{#if hasKids}
				<Collapsible.Trigger
					class="flex size-5 shrink-0 items-center justify-center rounded text-muted-foreground hover:text-foreground"
					aria-label={isOpen ? 'Collapse' : 'Expand'}
					data-testid={`folder-toggle-${f.slug}`}
				>
					<ChevronRight class="size-3.5 transition-transform {isOpen ? 'rotate-90' : ''}" />
				</Collapsible.Trigger>
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
			{#if actions}
				<span
					class="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100"
				>
					{@render actions(f)}
				</span>
			{/if}
		</div>
		{#if hasKids}
			<Collapsible.Content>
				<div class="ml-[1.4rem] border-l border-border/60 pl-1">
					{#each kids as child (child.id)}
						{@render node(child, depth + 1)}
					{/each}
				</div>
			</Collapsible.Content>
		{/if}
	</Collapsible.Root>
{/snippet}

<TreeView.Root class="gap-0.5" data-testid="folder-tree">
	{#if showRoot}
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
			{rootLabel}
		</button>
	{/if}
	{#each roots as root (root.id)}
		{@render node(root, 0)}
	{:else}
		<p class="px-2 py-1 text-sm text-muted-foreground/60 italic">No folders yet</p>
	{/each}
</TreeView.Root>
