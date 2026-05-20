<script lang="ts">
	import {
		SvelteFlow,
		Background,
		Controls,
		MiniMap,
		type Node,
		type Edge
	} from '@xyflow/svelte';
	import '@xyflow/svelte/dist/style.css';
	import type { AncestryNode, CrossNetEdge } from '$lib/api/client';
	import type { ProvenanceGraphNode } from '$lib/types/provenance';
	import { buildProvenanceGraph, layoutProvenanceGraph } from '$lib/utils/provenance-graph';
	import CausalityNode from './CausalityNode.svelte';
	import EventDetailSheet from './EventDetailSheet.svelte';

	interface Props {
		ancestry: AncestryNode[];
		crossNetEdges?: CrossNetEdge[];
	}

	let { ancestry, crossNetEdges = [] }: Props = $props();

	let nodes = $state<Node[]>([]);
	let edges = $state<Edge[]>([]);
	let selectedNode = $state<ProvenanceGraphNode | null>(null);
	let sheetOpen = $state(false);

	const nodeTypes = {
		causality: CausalityNode
	};

	$effect(() => {
		if (ancestry.length === 0) {
			nodes = [];
			edges = [];
			return;
		}

		const graph = buildProvenanceGraph(ancestry, false, crossNetEdges);
		const layout = layoutProvenanceGraph(graph.nodes, graph.edges);

		// Inject the onSelect callback into each node's data
		nodes = layout.nodes.map((n) => ({
			...n,
			data: {
				...n.data,
				onSelect: (node: ProvenanceGraphNode) => {
					selectedNode = node;
					sheetOpen = true;
				}
			}
		}));
		edges = layout.edges;
	});

	// Distinct net IDs for the legend
	const netIds = $derived([...new Set(ancestry.map((n) => n.net_id))]);
</script>

<div class="relative h-full w-full">
	{#if ancestry.length === 0}
		<div class="flex h-full items-center justify-center text-zinc-400">
			No provenance data available for this artifact.
		</div>
	{:else}
		<SvelteFlow {nodes} {edges} {nodeTypes} fitView colorMode="light" minZoom={0.1}>
			<Background />
			<Controls />
			<MiniMap />
		</SvelteFlow>

		<!-- Net legend -->
		{#if netIds.length > 1}
			<div class="absolute bottom-4 left-4 rounded-md border bg-white/90 px-3 py-2 text-sm shadow-sm dark:bg-zinc-900/90">
				<div class="font-semibold text-zinc-500 mb-1">Nets</div>
				{#each netIds as netId}
					<div class="text-zinc-600 dark:text-zinc-300 truncate max-w-[200px]">{netId}</div>
				{/each}
			</div>
		{/if}
	{/if}
</div>

<EventDetailSheet node={selectedNode} open={sheetOpen} onclose={() => { sheetOpen = false; selectedNode = null; }} />
