<script lang="ts">
	import {
		SvelteFlow,
		Controls,
		Background,
		MiniMap,
		BackgroundVariant,
		type Node,
		type Edge,
		type Connection
	} from '@xyflow/svelte';
	import '@xyflow/svelte/dist/style.css';

	import { nodeTypes } from './nodes';
	import NodePalette from './NodePalette.svelte';
	import DropHandler from './DropHandler.svelte';
	import {
		createDefaultNodeData,
		type WorkflowNodeData,
		type WorkflowNodeType,
		type WorkflowGraph
	} from '$lib/types/editor';
	import type { XYPosition } from '@xyflow/svelte';

	type Props = {
		graph: WorkflowGraph;
		readonly?: boolean;
		onchange?: (graph: WorkflowGraph) => void;
		onselect?: (nodeId: string | null) => void;
	};

	let { graph, readonly = false, onchange, onselect }: Props = $props();

	// Track graph identity to avoid re-syncing our own changes
	let lastGraphRef: WorkflowGraph | null = graph;

	let nodes = $state.raw<Node[]>(toFlowNodes(graph));
	let edges = $state.raw<Edge[]>(toFlowEdges(graph));

	function toFlowNodes(g: WorkflowGraph): Node[] {
		return g.nodes.map((n) => ({
			id: n.id,
			type: n.type,
			position: n.position,
			data: n.data
		}));
	}

	function toFlowEdges(g: WorkflowGraph): Edge[] {
		return g.edges.map((e) => ({
			id: e.id,
			source: e.source,
			target: e.target,
			sourceHandle: e.sourceHandle,
			label: e.label,
			type: 'default' as const,
			animated: e.type === 'loop_back'
		}));
	}

	// Re-sync from parent only when graph prop changes externally
	$effect.pre(() => {
		if (graph !== lastGraphRef) {
			lastGraphRef = graph;
			const currentNodes = new Map(nodes.map((n) => [n.id, n]));
			nodes = graph.nodes.map((n) => {
				const existing = currentNodes.get(n.id);
				return {
					id: n.id,
					type: n.type,
					position: existing?.position ?? n.position,
					data: n.data,
					...(existing?.selected != null ? { selected: existing.selected } : {})
				};
			});
			edges = toFlowEdges(graph);
		}
	});

	function serializeAndEmit() {
		const serialized: WorkflowGraph = {
			nodes: nodes.map((n) => ({
				id: n.id,
				type: n.type as WorkflowNodeType,
				position: n.position,
				data: n.data as WorkflowNodeData
			})),
			edges: edges.map((e) => ({
				id: e.id,
				source: e.source,
				target: e.target,
				sourceHandle: e.sourceHandle as string | undefined,
				label: typeof e.label === 'string' ? e.label : undefined,
				type: e.animated ? ('loop_back' as const) : ('sequence' as const)
			}))
		};
		lastGraphRef = serialized;
		onchange?.(serialized);
	}

	function onConnect(connection: Connection) {
		if (readonly) return;
		const newEdge: Edge = {
			id: `e-${connection.source}-${connection.target}-${Date.now()}`,
			source: connection.source!,
			target: connection.target!,
			sourceHandle: connection.sourceHandle,
			targetHandle: connection.targetHandle,
			type: 'default'
		};
		edges = [...edges, newEdge];
		serializeAndEmit();
	}

	function handleSelectionChange({ nodes: selectedNodes }: { nodes: Node[] }) {
		if (selectedNodes.length === 1) {
			onselect?.(selectedNodes[0].id);
		} else {
			onselect?.(null);
		}
	}

	function handleNodeDragStop() {
		serializeAndEmit();
	}

	function handleDelete({ nodes: deletedNodes }: { nodes: Node[] }) {
		if (readonly) return;
		const idsToDelete = new Set(deletedNodes.map((n) => n.id));
		nodes = nodes.filter((n) => !idsToDelete.has(n.id));
		edges = edges.filter((e) => !idsToDelete.has(e.source) && !idsToDelete.has(e.target));
		serializeAndEmit();
	}

	// Flow helpers provided by DropHandler child (inside SvelteFlow context)
	let screenToFlowPos: ((pos: { x: number; y: number }) => XYPosition) | null = null;

	function handleFlowInit(helpers: { screenToFlowPosition: (pos: { x: number; y: number }) => XYPosition }) {
		screenToFlowPos = helpers.screenToFlowPosition;
	}

	function onDrop(event: DragEvent) {
		if (readonly || !event.dataTransfer || !screenToFlowPos) return;

		const nodeType = event.dataTransfer.getData('application/mekhan-node-type') as WorkflowNodeType;
		if (!nodeType) return;

		event.preventDefault();

		const position = screenToFlowPos({ x: event.clientX, y: event.clientY });
		const newNode: Node = {
			id: `node-${Date.now()}`,
			type: nodeType,
			position,
			data: createDefaultNodeData(nodeType)
		};

		nodes = [...nodes, newNode];
		serializeAndEmit();
	}

	function onDragOver(event: DragEvent) {
		event.preventDefault();
		if (event.dataTransfer) {
			event.dataTransfer.dropEffect = 'move';
		}
	}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="flex" style="width: 100%; height: 100%;">
	{#if !readonly}
		<NodePalette />
	{/if}
	<div style="flex: 1; height: 100%; position: relative;" ondrop={onDrop} ondragover={onDragOver}>
		<SvelteFlow
			{nodeTypes}
			bind:nodes
			bind:edges
			onconnect={onConnect}
			onselectionchange={handleSelectionChange}
			ondelete={handleDelete}
			onnodedragstop={handleNodeDragStop}
			fitView
			deleteKey={["Backspace", "Delete"]}
			proOptions={{ hideAttribution: true }}
		>
			<DropHandler oninit={handleFlowInit} />
			<Controls position="bottom-right" />
			<Background variant={BackgroundVariant.Dots} gap={20} size={1} />
			<MiniMap position="bottom-left" />
		</SvelteFlow>
	</div>
</div>
