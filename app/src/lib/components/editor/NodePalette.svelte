<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import GitFork from '@lucide/svelte/icons/git-fork';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import { SvelteSet } from 'svelte/reactivity';
	import { forkLibraryNode } from '$lib/api/client';
	import { loadNodeTypes, nodeList } from '$lib/editor/node-registry.svelte';
	import {
		loadLibraryNodes,
		libraryNodeList,
		recentCoordinates,
		type LibraryNodeDescriptor
	} from '$lib/editor/library-registry.svelte';
	import { nodePaletteMeta } from '$lib/editor/node-palette-meta';
	import NodeIcon from '$lib/editor/NodeIcon.svelte';
	import type { WorkflowNodeType } from '$lib/types/editor';

	onMount(() => {
		// Idempotent — the +layout.svelte warmup already fired, but if a route
		// renders the palette before the layout resolves we fetch here too.
		loadNodeTypes().catch(() => {
			/* surfaced via empty list */
		});
		loadLibraryNodes().catch(() => {
			/* surfaced via empty Library section */
		});
	});

	const primitives = $derived(nodeList());
	const library = $derived(libraryNodeList());

	// Case-insensitive substring filter — label + wire-name for primitives;
	// name + coordinate + vendor + category for library nodes.
	let query = $state('');
	const q = $derived(query.trim().toLowerCase());

	// Active category facet — null = all categories. Click a chip to narrow.
	let activeCategory = $state<string | null>(null);
	// Collapsed groups, keyed by `cat` (category header) and `cat\0vendor`
	// (vendor sub-header). Membership = COLLAPSED; absent = expanded. A Set so the
	// default (expand-all) needs no pre-population and toggling is O(1).
	// SvelteSet from 'svelte/reactivity' so .add/.delete are reactive.
	let collapsed = $state(new SvelteSet<string>());

	const filteredPrimitives = $derived.by(() => {
		if (!q) return primitives;
		return primitives.filter(
			(i) => i.displayLabel.toLowerCase().includes(q) || i.wireName.toLowerCase().includes(q)
		);
	});

	// Category chip vocab, derived from the loaded (ACL-filtered) catalogue so it
	// only ever offers categories the caller can actually see. Alphabetised.
	const libraryCategories = $derived.by(() => {
		const set = new Set<string>();
		for (const n of library) set.add(n.presentation?.category ?? 'Other');
		return [...set].sort((a, b) => a.localeCompare(b));
	});

	const filteredLibrary = $derived.by(() => {
		let items = library;
		if (activeCategory) {
			items = items.filter((n) => (n.presentation?.category ?? 'Other') === activeCategory);
		}
		if (!q) return items;
		return items.filter(
			(n) =>
				n.name.toLowerCase().includes(q) ||
				n.coordinate.toLowerCase().includes(q) ||
				(n.presentation?.vendor ?? '').toLowerCase().includes(q) ||
				(n.presentation?.category ?? '').toLowerCase().includes(q)
		);
	});

	// Library grouping: category → vendor → items, alphabetised at each level
	// (the API already orders this way, but re-group defensively for the filter).
	const libraryGroups = $derived.by(() => {
		const byCat = new Map<string, Map<string, LibraryNodeDescriptor[]>>();
		for (const n of filteredLibrary) {
			const cat = n.presentation?.category ?? 'Other';
			const vendor = n.presentation?.vendor ?? 'Unknown';
			if (!byCat.has(cat)) byCat.set(cat, new Map());
			const byVendor = byCat.get(cat)!;
			if (!byVendor.has(vendor)) byVendor.set(vendor, []);
			byVendor.get(vendor)!.push(n);
		}
		return [...byCat.entries()]
			.sort((a, b) => a[0].localeCompare(b[0]))
			.map(([category, vendors]) => ({
				category,
				vendors: [...vendors.entries()]
					.sort((a, b) => a[0].localeCompare(b[0]))
					.map(([vendor, items]) => ({ vendor, items }))
			}));
	});

	// Recently dropped library nodes, resolved against the (filtered) catalogue
	// so the Recent group respects both search and ACL visibility.
	const recentItems = $derived.by(() => {
		const byCoord = new Map(filteredLibrary.map((n) => [n.coordinate, n]));
		return recentCoordinates()
			.map((c) => byCoord.get(c))
			.filter((n): n is LibraryNodeDescriptor => !!n);
	});

	const nothingMatches = $derived(
		q.length > 0 && filteredPrimitives.length === 0 && filteredLibrary.length === 0
	);

	// When filtering/narrowed/few-groups, force-expand everything; otherwise honor
	// the user's `collapsed` set. `forceExpand` wins so a search never hides a hit.
	const forceExpand = $derived(
		q.length > 0 || activeCategory !== null || libraryGroups.length <= 2
	);

	function isOpen(key: string): boolean {
		return forceExpand || !collapsed.has(key);
	}
	function toggle(key: string) {
		// Only meaningful when not force-expanded; while forceExpand is on the
		// chevrons are inert (still rendered open). Toggling the underlying set is
		// harmless and is respected once forceExpand turns off.
		if (collapsed.has(key)) collapsed.delete(key);
		else collapsed.add(key);
	}

	// One-shot default: collapse all category groups on first load when the list
	// is long enough to need it. Runs once (guarded by `seededDefaults`).
	let seededDefaults = $state(false);
	$effect(() => {
		if (seededDefaults) return;
		if (libraryGroups.length > 2 && library.length > 0) {
			for (const g of libraryGroups) collapsed.add(g.category);
			seededDefaults = true;
		} else if (library.length > 0) {
			seededDefaults = true; // small list: leave expanded, don't re-seed
		}
	});

	function onPrimitiveDragStart(event: DragEvent, nodeType: WorkflowNodeType) {
		if (!event.dataTransfer) return;
		event.dataTransfer.setData('application/mekhan-node-type', nodeType);
		event.dataTransfer.effectAllowed = 'move';
	}

	function onLibraryDragStart(event: DragEvent, node: LibraryNodeDescriptor) {
		if (!event.dataTransfer) return;
		// A library node drops as a `sub_workflow` node — the canvas reads the
		// node-type for the base shape and the library payload to pre-pin the
		// version + stamp the frozen presentation (decision 12).
		event.dataTransfer.setData('application/mekhan-node-type', 'sub_workflow');
		event.dataTransfer.setData('application/mekhan-library-node', JSON.stringify(node));
		event.dataTransfer.effectAllowed = 'move';
		// NB: do NOT mutate "recent" here — a dragstart-time state change
		// re-renders the palette and detaches the in-flight dragged element,
		// hanging the drag. The canvas marks it used on a successful drop.
	}

	// Fork-to-workspace (decision 5): deep-copy the library node into a new
	// editable private template the caller owns, then open its editor.
	let forking = $state<string | null>(null);
	async function onFork(node: LibraryNodeDescriptor) {
		if (forking) return;
		forking = node.coordinate;
		try {
			const forked = await forkLibraryNode(node.coordinate);
			await goto(`/templates/${forked.id}`);
		} catch {
			// Surfaced by the destination load failing; reset so the user can retry.
			forking = null;
		}
	}
</script>

{#snippet libraryItem(node: LibraryNodeDescriptor)}
	{@const color = node.presentation?.color ?? '#14b8a6'}
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="group flex cursor-grab items-center gap-2.5 rounded-lg border border-transparent px-2.5 py-2 text-sm transition-colors hover:border-border hover:bg-accent active:cursor-grabbing"
		draggable="true"
		title={node.coordinate}
		data-testid="palette-library-{node.coordinate}"
		ondragstart={(e) => onLibraryDragStart(e, node)}
	>
		<div
			class="flex size-7 shrink-0 items-center justify-center rounded-md"
			style="background-color: {color}20; color: {color};"
		>
			<NodeIcon icon={node.presentation?.icon} class="size-4" />
		</div>
		<div class="min-w-0 flex-1">
			<div class="truncate text-sm font-medium text-foreground">{node.name}</div>
			<div class="truncate text-sm leading-tight text-muted-foreground">
				{node.coordinate}{node.lifecycleStatus === 'deprecated'
					? node.supersededBy
						? ` · deprecated → ${node.supersededBy}`
						: ' · deprecated'
					: ''}
			</div>
		</div>
		<!-- Fork-to-workspace: hidden until hover so it doesn't crowd the card.
		     Not draggable; stops propagation so a click never starts a drag. -->
		<button
			type="button"
			class="shrink-0 rounded p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-accent hover:text-foreground focus:opacity-100 group-hover:opacity-100 disabled:opacity-50"
			title="Fork to workspace"
			aria-label="Fork {node.name} to workspace"
			data-testid="palette-library-fork-{node.coordinate}"
			disabled={forking === node.coordinate}
			onclick={(e) => {
				e.stopPropagation();
				onFork(node);
			}}
			ondragstart={(e) => e.preventDefault()}
		>
			<GitFork class="size-3.5" />
		</button>
	</div>
{/snippet}

<div class="flex w-56 flex-col border-r border-sidebar-border bg-sidebar" data-testid="node-palette">
	<div class="border-b border-sidebar-border px-3 py-2.5">
		<h2 class="text-sm font-semibold uppercase tracking-wider text-muted-foreground">Blocks</h2>
	</div>
	<div class="border-b border-sidebar-border p-2">
		<input
			type="text"
			placeholder="Filter blocks…"
			bind:value={query}
			data-testid="palette-search"
			class="w-full rounded-md border border-input bg-background px-2 py-1 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
			onkeydown={(e) => {
				if (e.key === 'Escape') {
					query = '';
					// Keep Escape local — don't let it bubble into canvas/page
					// handlers (deselect, close panel) while clearing the filter.
					e.stopPropagation();
				}
			}}
		/>
	</div>
	<div class="flex-1 overflow-y-auto p-2">
		{#if nothingMatches}
			<p class="px-2.5 py-3 text-sm text-muted-foreground" data-testid="palette-empty">
				No blocks match “{query.trim()}”.
			</p>
		{/if}

		<!-- Primitives -->
		{#if filteredPrimitives.length > 0}
			<div
				class="px-2.5 pb-1 pt-1 text-xs font-semibold uppercase tracking-wider text-muted-foreground"
			>
				Primitives
			</div>
			<div class="space-y-1">
				{#each filteredPrimitives as item (item.wireName)}
					{@const meta = nodePaletteMeta(item.wireName)}
					{@const Icon = meta.icon}
					<!-- svelte-ignore a11y_no_static_element_interactions -->
					<div
						class="group flex cursor-grab items-center gap-2.5 rounded-lg border border-transparent px-2.5 py-2 text-sm transition-colors hover:border-border hover:bg-accent active:cursor-grabbing"
						draggable="true"
						data-testid="palette-item-{item.wireName}"
						ondragstart={(e) => onPrimitiveDragStart(e, item.wireName as WorkflowNodeType)}
					>
						<div
							class="flex size-7 shrink-0 items-center justify-center rounded-md"
							style="background-color: {meta.color}20; color: {meta.color};"
						>
							<Icon class="size-4" />
						</div>
						<div class="min-w-0">
							<div class="text-sm font-medium text-foreground">{item.displayLabel}</div>
							{#if item.description}
								<div class="truncate text-sm leading-tight text-muted-foreground">
									{item.description}
								</div>
							{/if}
						</div>
					</div>
				{/each}
			</div>
		{/if}

		<!-- Library -->
		{#if filteredLibrary.length > 0}
			<div
				class="mt-3 px-2.5 pb-1 pt-1 text-xs font-semibold uppercase tracking-wider text-muted-foreground"
				data-testid="palette-library-header"
			>
				Library
			</div>

			{#if libraryCategories.length > 1}
				<div class="flex flex-wrap gap-1 px-2.5 pb-1 pt-1" data-testid="palette-category-chips">
					<button
						type="button"
						class="rounded-full border px-2 py-0.5 text-[11px] transition-colors {activeCategory ===
						null
							? 'border-foreground bg-foreground text-background'
							: 'border-border text-muted-foreground hover:bg-accent'}"
						onclick={() => (activeCategory = null)}
						data-testid="palette-category-all">All</button
					>
					{#each libraryCategories as cat (cat)}
						<button
							type="button"
							class="rounded-full border px-2 py-0.5 text-[11px] transition-colors {activeCategory ===
							cat
								? 'border-foreground bg-foreground text-background'
								: 'border-border text-muted-foreground hover:bg-accent'}"
							onclick={() => (activeCategory = activeCategory === cat ? null : cat)}
							data-testid="palette-category-chip-{cat}">{cat}</button
						>
					{/each}
				</div>
			{/if}

			{#if recentItems.length > 0}
				<div class="px-2.5 pb-0.5 pt-1 text-xs font-medium text-muted-foreground/80">Recent</div>
				<div class="space-y-1">
					{#each recentItems as node (node.coordinate)}
						{@render libraryItem(node)}
					{/each}
				</div>
			{/if}

			{#each libraryGroups as group (group.category)}
				{@const catOpen = isOpen(group.category)}
				<button
					type="button"
					class="flex w-full items-center gap-1 px-2.5 pb-0.5 pt-2 text-left text-xs font-medium text-muted-foreground/80 hover:text-foreground"
					onclick={() => toggle(group.category)}
					aria-expanded={catOpen}
					data-testid="palette-category-group-{group.category}"
				>
					<ChevronRight class="size-3 shrink-0 transition-transform {catOpen ? 'rotate-90' : ''}" />
					<span class="truncate">{group.category}</span>
					<span class="ml-auto text-[11px] tabular-nums text-muted-foreground/50">
						{group.vendors.reduce((sum, v) => sum + v.items.length, 0)}
					</span>
				</button>
				{#if catOpen}
					{#each group.vendors as v (v.vendor)}
						{#if group.vendors.length > 1 || v.vendor !== 'Unknown'}
							{@const vKey = `${group.category}\0${v.vendor}`}
							{@const vOpen = isOpen(vKey)}
							<button
								type="button"
								class="flex w-full items-center gap-1 px-2.5 pb-0.5 pl-4 pt-0.5 text-left text-[11px] uppercase tracking-wide text-muted-foreground/60 hover:text-foreground"
								onclick={() => toggle(vKey)}
								aria-expanded={vOpen}
								data-testid="palette-vendor-group-{group.category}-{v.vendor}"
							>
								<ChevronRight
									class="size-3 shrink-0 transition-transform {vOpen ? 'rotate-90' : ''}"
								/>
								<span class="truncate">{v.vendor}</span>
							</button>
							{#if vOpen}
								<div class="space-y-1">
									{#each v.items as node (node.coordinate)}{@render libraryItem(node)}{/each}
								</div>
							{/if}
						{:else}
							<!-- single unnamed vendor: no sub-header, render items under the category -->
							<div class="space-y-1">
								{#each v.items as node (node.coordinate)}{@render libraryItem(node)}{/each}
							</div>
						{/if}
					{/each}
				{/if}
			{/each}
		{:else if activeCategory && library.length > 0}
			<p
				class="px-2.5 py-2 text-sm text-muted-foreground"
				data-testid="palette-library-category-empty"
			>
				No Library blocks in “{activeCategory}”.
			</p>
		{/if}
	</div>
</div>
