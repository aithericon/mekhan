<script lang="ts">
	// Producer → variable reference picker. A two-column popover:
	//   left  column = nodes that produce in-scope data, plus the synthetic
	//                   "Process" bucket for control/identity leaves
	//   right column = ONLY the selected node's variables
	// Replaces the flat grouped dropdown/chip list, which didn't scale past a
	// handful of nodes. A single filter narrows both columns at once.
	//
	// Resources tab: when the parent provides a non-empty `resourceScope`
	// (built from `WorkflowGraph.resources` + the type registry), the
	// popover gains a tab switcher. The Resources tab keeps the same
	// two-column shape, with alias buckets on the left and their public +
	// secret fields on the right. Picking a resource field emits a
	// regular `ScopeEntry` whose `qualified` is `<alias>.<field>` — the
	// compiler already discriminates alias-vs-slug, so callers don't
	// need to differentiate the two ref kinds.
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import * as Popover from '$lib/components/ui/popover';
	import { Input } from '$lib/components/ui/input';
	import { cn } from '$lib/utils.js';
	import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down';

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
		onpick: (entry: ScopeEntry) => void;
	};

	let {
		scope,
		resourceScope = [],
		disabled = false,
		selected,
		placeholder = 'Pick field…',
		triggerClass = '',
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

	// Default tab follows `selected`: if the picked ref is a resource entry,
	// open in the Resources tab on next render; otherwise stick with Refs.
	let activeTab = $state<Tab>('refs');
	$effect(() => {
		if (selected && resourceScope.some((e) => e.qualified === selected)) {
			activeTab = 'resources';
		}
	});

	let open = $state(false);
	let query = $state('');
	let activeKey = $state<string | null>(null);

	const q = $derived(query.trim().toLowerCase());

	const sourceGroups = $derived(activeTab === 'resources' ? resourceGroups : refGroups);

	// A group survives the filter if its label matches or any entry matches;
	// surviving groups keep only their matching entries.
	const visibleGroups = $derived.by(() => {
		if (!q) return sourceGroups;
		const out: Group[] = [];
		for (const g of sourceGroups) {
			const labelHit = g.label.toLowerCase().includes(q);
			const entries = g.entries.filter(
				(e) => e.field.toLowerCase().includes(q) || e.qualified.toLowerCase().includes(q)
			);
			if (labelHit || entries.length > 0) {
				out.push({ ...g, entries: labelHit ? g.entries : entries });
			}
		}
		return out;
	});

	// Drop stale `activeKey` when switching tabs — a key from the other tab
	// would resolve to nothing in this tab's groups.
	$effect(() => {
		void activeTab;
		activeKey = null;
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

	const activeEntries = $derived(activeGroup?.entries ?? []);

	const emptyMessage = $derived.by(() => {
		if (activeTab === 'resources') {
			return resourceScope.length === 0
				? 'No resources declared on this workflow.'
				: 'No matching resource fields.';
		}
		return scope.length === 0 ? 'No upstream fields in scope.' : 'No matching fields.';
	});

	$effect(() => {
		if (!open) query = '';
	});

	function pick(e: ScopeEntry) {
		onpick(e);
		open = false;
	}
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
			<!-- Tab switcher. Resources is a peer category, not a subtab of refs,
			     so the bar sits above the filter and the active state is bold. -->
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
								<span
									class={cn('truncate', g.isProcess && 'text-muted-foreground italic')}
								>
									{g.label}
								</span>
								<span class="shrink-0 text-sm text-muted-foreground">{g.entries.length}</span>
							</button>
						</li>
					{/each}
				</ul>

				<!-- Variable selection column (selected node only) -->
				<ul class="flex-1 overflow-y-auto py-1">
					{#each activeEntries as e (e.qualified)}
						<li>
							<button
								type="button"
								class={cn(
									'flex w-full items-center justify-between gap-3 px-3 py-2 text-left transition-colors hover:bg-accent',
									selected === e.qualified && 'bg-accent'
								)}
								onclick={() => pick(e)}
								title={`${e.nodeLabel} → ${e.field} (${e.kind})`}
							>
								<span class="truncate font-mono text-sm">{e.qualified}</span>
								<span class="shrink-0 text-sm text-muted-foreground">{e.kind}</span>
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
