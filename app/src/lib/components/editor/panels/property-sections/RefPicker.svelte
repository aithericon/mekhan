<script lang="ts">
	// Producer → variable reference picker. A two-column popover:
	//   left  column = nodes that produce in-scope data, plus the synthetic
	//                   "Process" bucket for control/identity leaves
	//   right column = ONLY the selected node's variables, rendered as a
	//                   recursive tree so nested object fields (File
	//                   envelopes `document.url`, …) and (in feature B)
	//                   array element fields are pickable without leaving
	//                   the popover. A single filter narrows both columns
	//                   at once; ancestors of matches auto-expand.
	//
	// Resources tab: when the parent provides a non-empty `resourceScope`
	// (built from `WorkflowGraph.resources` + the type registry), the
	// popover gains a tab switcher. The Resources tab keeps the same
	// two-column shape; resource entries have no `ty` tree, so the right
	// column flattens to one row per field (the legacy shape).
	import type { ScopeEntry, TyDescriptor } from '$lib/editor/guard-scope';
	import { tyDescriptorLabel } from '$lib/editor/guard-scope';
	import * as Popover from '$lib/components/ui/popover';
	import { Input } from '$lib/components/ui/input';
	import { cn } from '$lib/utils.js';
	import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down';
	import ChevronRight from '@lucide/svelte/icons/chevron-right';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';

	type Props = {
		scope: ScopeEntry[];
		/** Workflow-level resource refs (alias → field) flattened to
		 *  `ScopeEntry[]`. When non-empty the popover renders tabs and
		 *  the user can switch between in-scope refs and resources. */
		resourceScope?: ScopeEntry[];
		disabled?: boolean;
		/** Currently-picked qualified ref, shown in the trigger + highlighted. */
		selected?: string;
		placeholder?: string;
		triggerClass?: string;
		/** Feature B — when `true`, array-typed rows offer a synthetic
		 *  `[*]` child the user can drill into for `<slug>.<field>[*].<sub>`
		 *  iteration refs. Default `false`: aggregations and per-element
		 *  borrows are non-goals for guards/conditions/result-mapping.
		 *  The Repeater config UI sets this to `true` for its `items_ref`
		 *  picker. */
		allowArrayBoundary?: boolean;
		onpick: (entry: ScopeEntry) => void;
	};

	let {
		scope,
		resourceScope = [],
		disabled = false,
		selected,
		placeholder = 'Pick field…',
		triggerClass = '',
		allowArrayBoundary = false,
		onpick
	}: Props = $props();

	type Tab = 'refs' | 'resources';
	type Group = { key: string; label: string; isProcess: boolean; entries: ScopeEntry[] };

	// Group by producer (stable first-seen order), keyed by node id + label so
	// two distinctly-attributed producers never merge. The synthetic
	// "Process" bucket is forced last — control/identity, not business data.
	function makeGroups(entries: ScopeEntry[]): Group[] {
		const out: Group[] = [];
		for (const e of entries) {
			const key = `${e.nodeId} ${e.nodeLabel}`;
			let g = out.find((x) => x.key === key);
			if (!g) {
				g = { key, label: e.nodeLabel, isProcess: e.nodeLabel === 'Process', entries: [] };
				out.push(g);
			}
			g.entries.push(e);
		}
		return out.sort((a, b) => Number(a.isProcess) - Number(b.isProcess));
	}

	const refGroups = $derived(makeGroups(scope));
	const resourceGroups = $derived(makeGroups(resourceScope));

	const hasResources = $derived(resourceScope.length > 0);

	let activeTab = $state<Tab>('refs');
	$effect(() => {
		if (selected && resourceScope.some((e) => e.qualified === selected)) {
			activeTab = 'resources';
		}
	});

	let open = $state(false);
	let query = $state('');
	let activeKey = $state<string | null>(null);
	let expanded = $state<Set<string>>(new Set());
	let hoveredPath = $state<string | null>(null);

	const q = $derived(query.trim().toLowerCase());
	const sourceGroups = $derived(activeTab === 'resources' ? resourceGroups : refGroups);

	// A group survives the filter if its label matches or any reachable path
	// (including nested object fields) matches. Surviving groups keep the
	// full entry list so the tree builder can still walk into ancestors of
	// matches; the per-entry visibility is decided in `buildTree`.
	const visibleGroups = $derived.by(() => {
		if (!q) return sourceGroups;
		const out: Group[] = [];
		for (const g of sourceGroups) {
			const labelHit = g.label.toLowerCase().includes(q);
			const entries = g.entries.filter((e) => entryMatchesFilter(e, q));
			if (labelHit || entries.length > 0) {
				out.push({ ...g, entries: labelHit ? g.entries : entries });
			}
		}
		return out;
	});

	function entryMatchesFilter(e: ScopeEntry, query: string): boolean {
		if (e.qualified.toLowerCase().includes(query)) return true;
		if (e.field.toLowerCase().includes(query)) return true;
		if (e.ty) return tyHasMatchAnywhere(e.qualified, e.ty, query);
		return false;
	}

	function tyHasMatchAnywhere(path: string, ty: TyDescriptor, query: string): boolean {
		if (path.toLowerCase().includes(query)) return true;
		if (ty.kind === 'object') {
			for (const [k, v] of Object.entries(ty.fields)) {
				if (tyHasMatchAnywhere(`${path}.${k}`, v, query)) return true;
			}
		}
		// Feature B: when iteration boundaries are allowed, the synthetic
		// `[*]` is part of the addressable surface — walk into the
		// element shape so filter auto-expand crosses the boundary.
		if (ty.kind === 'array' && allowArrayBoundary) {
			return tyHasMatchAnywhere(`${path}[*]`, ty.element, query);
		}
		return false;
	}

	$effect(() => {
		void activeTab;
		activeKey = null;
		expanded = new Set();
		hoveredPath = null;
	});

	const activeGroup = $derived.by(() => {
		const list = visibleGroups;
		if (list.length === 0) return null;
		const byKey = activeKey ? list.find((g) => g.key === activeKey) : undefined;
		if (byKey) return byKey;
		if (selected) {
			const owner = list.find((g) => g.entries.some((e) => e.qualified === selected));
			if (owner) return owner;
		}
		return list[0];
	});

	type TreeRow = {
		path: string;
		field: string;
		ty: TyDescriptor | null;
		depth: number;
		selectable: boolean;
		hasChildren: boolean;
		root: ScopeEntry;
		dimmed: boolean;
	};

	function isSelectable(ty: TyDescriptor | null): boolean {
		if (!ty) return true; // legacy resource entries have no ty — treat as terminal
		switch (ty.kind) {
			case 'scalar':
			case 'any':
			case 'opaque':
				return true;
			case 'object':
				return ty.selectable;
			case 'array':
				// Whole-array selection is intentionally NOT offered — aggregation
				// over array values is a non-goal (use a Python step downstream).
				// The synthetic `[*]` child (when `allowArrayBoundary` is on) is
				// the pickable iteration boundary.
				return false;
		}
	}

	function hasChildren(ty: TyDescriptor | null): boolean {
		if (!ty) return false;
		if (ty.kind === 'object') return Object.keys(ty.fields).length > 0;
		// Feature B: arrays expose one synthetic `[*]` child when the
		// caller has opted into iteration boundaries (the Repeater
		// config). Other consumers see arrays as terminal-but-unselectable
		// rows so they can't accidentally pick a wildcard.
		if (ty.kind === 'array' && allowArrayBoundary) return true;
		return false;
	}

	const treeRows = $derived.by(() => {
		const out: TreeRow[] = [];
		const grp = activeGroup;
		if (!grp) return out;
		for (const e of grp.entries) {
			walkEntry(e, out);
		}
		return out;
	});

	function walkEntry(e: ScopeEntry, out: TreeRow[]) {
		walkTy(e, e.qualified, e.field, e.ty ?? null, 0, out);
	}

	function walkTy(
		root: ScopeEntry,
		path: string,
		field: string,
		ty: TyDescriptor | null,
		depth: number,
		out: TreeRow[]
	) {
		const matched = !q || path.toLowerCase().includes(q) || field.toLowerCase().includes(q);
		const childMatches = ty ? tyHasMatchAnywhere(path, ty, q) : false;
		// With an active filter, hide branches that contain no match anywhere.
		if (q && !matched && !childMatches) return;
		const rowHasChildren = hasChildren(ty);
		out.push({
			path,
			field,
			ty,
			depth,
			selectable: isSelectable(ty),
			hasChildren: rowHasChildren,
			root,
			dimmed: !!q && !matched && childMatches
		});
		// Auto-expand on active filter so the matched descendant is visible
		// without a click; otherwise honour the user's explicit expand set.
		const shouldExpand = expanded.has(path) || (!!q && childMatches);
		if (shouldExpand && rowHasChildren && ty) {
			if (ty.kind === 'object') {
				for (const [k, v] of Object.entries(ty.fields)) {
					walkTy(root, `${path}.${k}`, k, v, depth + 1, out);
				}
			} else if (ty.kind === 'array' && allowArrayBoundary) {
				// Feature B: synthetic `[*]` child marks the iteration
				// boundary. The boundary itself is always pickable (Repeater
				// items_ref binds to it directly, e.g. `extract.tasks[*]`),
				// regardless of the element type's selectability — the
				// "wholeness" of the boundary trumps per-element field
				// pickability. The synthetic row's own children are then
				// the element's fields (drilled into via the standard
				// Object recursion).
				const elem = ty.element;
				const boundaryPath = `${path}[*]`;
				const boundaryHasChildren =
					elem.kind === 'object' && Object.keys(elem.fields).length > 0;
				out.push({
					path: boundaryPath,
					field: '[*]',
					ty: elem,
					depth: depth + 1,
					selectable: true,
					hasChildren: boundaryHasChildren,
					root,
					dimmed: false
				});
				// Auto-expand the boundary on filter match so element
				// fields are visible; otherwise honour the explicit set.
				const expandBoundary =
					expanded.has(boundaryPath) ||
					(!!q && elem.kind === 'object' && tyHasMatchAnywhere(boundaryPath, elem, q));
				if (expandBoundary && elem.kind === 'object') {
					for (const [k, v] of Object.entries(elem.fields)) {
						walkTy(root, `${boundaryPath}.${k}`, k, v, depth + 2, out);
					}
				}
			}
		}
	}

	function toggle(path: string) {
		const next = new Set(expanded);
		if (next.has(path)) next.delete(path);
		else next.add(path);
		expanded = next;
	}

	function rowClick(row: TreeRow) {
		if (row.hasChildren && !row.selectable) {
			toggle(row.path);
			return;
		}
		if (!row.selectable) return;
		// Synthesize the emitted entry: keep the producer attribution from
		// the root, but rewrite `qualified`/`field`/`ty`/`kind` to the
		// picked nested leaf so callers (guards, mapping panels) receive
		// what they expect.
		const synthesized: ScopeEntry = {
			nodeId: row.root.nodeId,
			nodeLabel: row.root.nodeLabel,
			field: row.field,
			kind: row.root.kind, // legacy FieldKind from the root; sufficient for callers
			qualified: row.path,
			ty: row.ty ?? undefined
		};
		onpick(synthesized);
		open = false;
	}

	const emptyMessage = $derived.by(() => {
		if (activeTab === 'resources') {
			return resourceScope.length === 0
				? 'No resources declared on this workflow.'
				: 'No matching resource fields.';
		}
		return scope.length === 0 ? 'No upstream fields in scope.' : 'No matching fields.';
	});

	$effect(() => {
		if (!open) {
			query = '';
			expanded = new Set();
			hoveredPath = null;
		}
	});
</script>

<Popover.Root bind:open>
	<Popover.Trigger
		{disabled}
		class={cn(
			'flex h-9 w-full items-center justify-between gap-1.5 rounded-md border border-input bg-input px-3 text-sm shadow-xs outline-none transition-colors hover:bg-accent disabled:cursor-not-allowed disabled:opacity-50',
			triggerClass
		)}
	>
		{#if selected}
			<span class="truncate font-mono">{selected}</span>
		{:else}
			<span class="text-muted-foreground"
				>{scope.length === 0 && !hasResources ? 'No scope' : placeholder}</span
			>
		{/if}
		<ChevronsUpDown class="size-4 shrink-0 opacity-50" />
	</Popover.Trigger>

	<Popover.Content align="start" class="w-[620px] max-w-[90vw] overflow-hidden p-0">
		{#if hasResources}
			<div class="flex border-b" role="tablist" data-testid="ref-picker-tabs">
				<button
					type="button"
					role="tab"
					aria-selected={activeTab === 'refs'}
					class={cn(
						'flex-1 px-3 py-2 text-sm transition-colors hover:bg-accent',
						activeTab === 'refs'
							? 'border-b-2 border-foreground font-medium text-foreground'
							: 'text-muted-foreground'
					)}
					onclick={() => (activeTab = 'refs')}
					data-testid="ref-picker-tab-refs"
				>
					Refs
					<span class="ml-1.5 text-muted-foreground">({scope.length})</span>
				</button>
				<button
					type="button"
					role="tab"
					aria-selected={activeTab === 'resources'}
					class={cn(
						'flex-1 px-3 py-2 text-sm transition-colors hover:bg-accent',
						activeTab === 'resources'
							? 'border-b-2 border-foreground font-medium text-foreground'
							: 'text-muted-foreground'
					)}
					onclick={() => (activeTab = 'resources')}
					data-testid="ref-picker-tab-resources"
				>
					Resources
					<span class="ml-1.5 text-muted-foreground">({resourceScope.length})</span>
				</button>
			</div>
		{/if}

		<div class="border-b p-3">
			<Input
				type="text"
				value={query}
				placeholder={activeTab === 'resources'
					? 'Filter aliases & fields…'
					: 'Filter nodes & fields…'}
				oninput={(e) => (query = (e.currentTarget as HTMLInputElement).value)}
				class="h-9 text-sm"
			/>
		</div>

		{#if visibleGroups.length === 0}
			<div class="p-4 text-sm italic text-muted-foreground">{emptyMessage}</div>
		{:else}
			<!-- Breadcrumb above the field column: shows the path the user is
			     about to act on (hover / focus). Empty when nothing is
			     highlighted so the layout doesn't jump. -->
			{#if hoveredPath}
				<div
					class="border-b bg-muted/30 px-3 py-1.5 font-mono text-xs text-muted-foreground"
					data-testid="ref-picker-breadcrumb"
				>
					{hoveredPath}
				</div>
			{/if}
			<div class="flex h-80">
				<!-- Producer / alias column -->
				<ul class="w-60 shrink-0 overflow-y-auto border-r py-1">
					{#each visibleGroups as g (g.key)}
						<li>
							<button
								type="button"
								class={cn(
									'flex w-full items-center justify-between gap-2 px-3 py-2 text-left text-sm transition-colors hover:bg-accent',
									activeGroup?.key === g.key && 'bg-accent font-medium'
								)}
								onmouseenter={() => (activeKey = g.key)}
								onfocus={() => (activeKey = g.key)}
								onclick={() => (activeKey = g.key)}
							>
								<span class={cn('truncate', g.isProcess && 'text-muted-foreground italic')}>
									{g.label}
								</span>
								<span class="shrink-0 text-sm text-muted-foreground">{g.entries.length}</span>
							</button>
						</li>
					{/each}
				</ul>

				<!-- Variable selection column (recursive tree, selected node only).
				     The chevron and the row body are sibling buttons rather
				     than nested (which the HTML spec forbids and Svelte's
				     hydration check flags). -->
				<ul class="flex-1 overflow-y-auto py-1">
					{#each treeRows as row (row.path)}
						<li
							class={cn(
								'flex items-center gap-2 transition-colors hover:bg-accent',
								selected === row.path && 'bg-accent',
								row.dimmed && 'opacity-50'
							)}
							onmouseenter={() => (hoveredPath = row.path)}
							onfocusin={() => (hoveredPath = row.path)}
							role="presentation"
						>
							<div
								class="flex shrink-0 items-center justify-center"
								style:padding-left={`${12 + row.depth * 16}px`}
							>
								{#if row.hasChildren}
									<button
										type="button"
										class="-mx-0.5 inline-flex size-5 items-center justify-center rounded hover:bg-muted"
										onclick={() => toggle(row.path)}
										aria-label={expanded.has(row.path) ? 'Collapse' : 'Expand'}
										data-testid={`ref-picker-toggle-${row.path}`}
									>
										{#if expanded.has(row.path) || (q && tyHasMatchAnywhere(row.path, row.ty!, q) && !row.path.toLowerCase().includes(q))}
											<ChevronDown class="size-3" />
										{:else}
											<ChevronRight class="size-3" />
										{/if}
									</button>
								{:else}
									<span class="inline-block size-5"></span>
								{/if}
							</div>
							<button
								type="button"
								class={cn(
									'flex flex-1 items-center gap-2 py-1.5 pr-3 text-left',
									!row.selectable && row.hasChildren && 'cursor-pointer',
									!row.selectable && !row.hasChildren && 'cursor-default'
								)}
								onclick={() => rowClick(row)}
								title={`${row.root.nodeLabel} → ${row.field}`}
								data-testid={`ref-picker-row-${row.path}`}
							>
								<span class="truncate font-mono text-sm">{row.field}</span>
								<span class="ml-auto shrink-0 text-xs text-muted-foreground">
									{tyDescriptorLabel(row.ty ?? undefined)}
								</span>
							</button>
						</li>
					{:else}
						<li class="px-3 py-2 text-sm italic text-muted-foreground">No variables.</li>
					{/each}
				</ul>
			</div>
		{/if}
	</Popover.Content>
</Popover.Root>
