<script lang="ts">
	import { onMount } from 'svelte';
	import { loadNodeTypes, nodeList } from '$lib/editor/node-registry.svelte';
	import {
		loadLibraryNodes,
		libraryNodeList,
		recentCoordinates,
		type LibraryNodeDescriptor
	} from '$lib/editor/library-registry.svelte';
	import { nodePaletteMeta } from '$lib/editor/node-palette-meta';
	import { resolveNodeIcon } from '$lib/editor/icon-registry';
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

	const filteredPrimitives = $derived.by(() => {
		if (!q) return primitives;
		return primitives.filter(
			(i) => i.displayLabel.toLowerCase().includes(q) || i.wireName.toLowerCase().includes(q)
		);
	});

	const filteredLibrary = $derived.by(() => {
		if (!q) return library;
		return library.filter(
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
</script>

{#snippet libraryItem(node: LibraryNodeDescriptor)}
	{@const Icon = resolveNodeIcon(node.presentation?.icon)}
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
			<Icon class="size-4" />
		</div>
		<div class="min-w-0 flex-1">
			<div class="truncate text-sm font-medium text-foreground">{node.name}</div>
			<div class="truncate text-sm leading-tight text-muted-foreground">
				{node.coordinate}{node.lifecycleStatus === 'deprecated' ? ' · deprecated' : ''}
			</div>
		</div>
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

			{#if recentItems.length > 0}
				<div class="px-2.5 pb-0.5 pt-1 text-xs font-medium text-muted-foreground/80">Recent</div>
				<div class="space-y-1">
					{#each recentItems as node (node.coordinate)}
						{@render libraryItem(node)}
					{/each}
				</div>
			{/if}

			{#each libraryGroups as group (group.category)}
				<div class="px-2.5 pb-0.5 pt-2 text-xs font-medium text-muted-foreground/80">
					{group.category}
				</div>
				{#each group.vendors as v (v.vendor)}
					{#if group.vendors.length > 1 || v.vendor !== 'Unknown'}
						<div class="px-2.5 pb-0.5 pt-0.5 text-[11px] uppercase tracking-wide text-muted-foreground/60">
							{v.vendor}
						</div>
					{/if}
					<div class="space-y-1">
						{#each v.items as node (node.coordinate)}
							{@render libraryItem(node)}
						{/each}
					</div>
				{/each}
			{/each}
		{/if}
	</div>
</div>
