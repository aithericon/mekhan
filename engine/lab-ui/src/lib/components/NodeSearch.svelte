<script lang="ts">
	import { useSvelteFlow } from '@xyflow/svelte';
	import { Search, X } from '@lucide/svelte';
	import type { PetriNet } from '$lib/api/client';

	interface Props {
		topology: PetriNet | null;
		onSelectPlace?: (id: string) => void;
		onSelectTransition?: (id: string) => void;
	}

	let { topology, onSelectPlace, onSelectTransition }: Props = $props();

	const { fitView, getNodes } = useSvelteFlow();

	let query = $state('');
	let open = $state(false);
	let inputEl: HTMLInputElement | undefined = $state();
	let selectedIdx = $state(0);

	const searchItems = $derived.by(() => {
		if (!topology) return [];
		const items: Array<{ id: string; name: string; type: 'place' | 'transition'; kind?: string }> = [];
		for (const p of topology.places) {
			items.push({ id: p.id, name: p.name, type: 'place', kind: (p as any).kind ?? 'internal' });
		}
		for (const t of topology.transitions) {
			items.push({ id: t.id, name: t.name, type: 'transition' });
		}
		return items;
	});

	const results = $derived.by(() => {
		if (!query.trim()) return [];
		const q = query.toLowerCase();
		return searchItems
			.filter(item => item.name.toLowerCase().includes(q))
			.slice(0, 10);
	});

	// Reset selected index when results change
	$effect(() => {
		results; // track
		selectedIdx = 0;
	});

	function selectResult(item: typeof searchItems[0]) {
		if (item.type === 'place') onSelectPlace?.(item.id);
		else onSelectTransition?.(item.id);

		const nodeId = item.type === 'place' ? `p:${item.id}` : `t:${item.id}`;
		const targetNodes = getNodes().filter(n => n.id === nodeId);
		if (targetNodes.length > 0) {
			fitView({ nodes: targetNodes, duration: 150, padding: 0.5 });
		}

		query = '';
		open = false;
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			open = false;
			query = '';
		} else if (e.key === 'ArrowDown') {
			e.preventDefault();
			selectedIdx = Math.min(selectedIdx + 1, results.length - 1);
		} else if (e.key === 'ArrowUp') {
			e.preventDefault();
			selectedIdx = Math.max(selectedIdx - 1, 0);
		} else if (e.key === 'Enter' && results.length > 0) {
			e.preventDefault();
			selectResult(results[selectedIdx]);
		}
	}

	function handleGlobalKeydown(e: KeyboardEvent) {
		if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
			e.preventDefault();
			e.stopPropagation();
			open = !open;
			if (open) requestAnimationFrame(() => inputEl?.focus());
		}
	}
</script>

<svelte:window onkeydown={handleGlobalKeydown} />

{#if open}
	<!-- svelte-ignore a11y_no_static_element_interactions -->
	<div
		class="absolute top-2 left-14 z-20 w-64"
		onkeydown={handleKeydown}
	>
		<div class="relative">
			<Search class="absolute left-2 top-2 w-4 h-4 text-muted-foreground pointer-events-none" />
			<input
				bind:this={inputEl}
				bind:value={query}
				placeholder="Search places & transitions..."
				class="w-full pl-8 pr-8 py-1.5 text-sm rounded-md border bg-card text-foreground shadow-lg focus:outline-none focus:ring-2 focus:ring-primary"
			/>
			<button
				class="absolute right-2 top-2 text-muted-foreground hover:text-foreground"
				onclick={() => { open = false; query = ''; }}
			>
				<X class="w-4 h-4" />
			</button>
		</div>

		{#if results.length > 0}
			<div class="mt-1 bg-card border rounded-md shadow-lg max-h-64 overflow-y-auto">
				{#each results as item, i (item.id)}
					<button
						class="w-full text-left px-3 py-2 text-sm flex items-center gap-2 transition-colors
							{i === selectedIdx ? 'bg-accent' : 'hover:bg-accent/50'}"
						onclick={() => selectResult(item)}
					>
						<span class="text-[10px] px-1 rounded font-mono {item.type === 'place' ? 'bg-blue-500/15 text-blue-500' : 'bg-gray-500/15 text-gray-500'}">
							{item.type === 'place' ? 'P' : 'T'}
						</span>
						<span class="truncate">{item.name}</span>
						{#if item.kind && item.kind !== 'internal'}
							<span class="text-[10px] text-muted-foreground ml-auto shrink-0">{item.kind}</span>
						{/if}
					</button>
				{/each}
			</div>
		{:else if query.trim()}
			<div class="mt-1 bg-card border rounded-md shadow-lg px-3 py-2 text-sm text-muted-foreground">
				No results
			</div>
		{/if}
	</div>
{:else}
	<button
		class="absolute top-2 left-14 z-20 p-1.5 rounded-md bg-card border shadow-sm text-muted-foreground hover:text-foreground hover:bg-accent transition-colors"
		onclick={() => { open = true; requestAnimationFrame(() => inputEl?.focus()); }}
		title="Search nodes (⌘K)"
	>
		<Search class="w-4 h-4" />
	</button>
{/if}
