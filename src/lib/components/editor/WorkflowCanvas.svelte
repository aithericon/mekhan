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
		type WorkflowEdge,
		type WorkflowGraph
	} from '$lib/types/editor';
	import type { XYPosition } from '@xyflow/svelte';

	type Props = {
		graph: WorkflowGraph;
		readonly?: boolean;
		onchange?: (graph: WorkflowGraph) => void;
		onselect?: (nodeId: string | null) => void;
		onAddNode?: (id: string, type: WorkflowNodeType, position: { x: number; y: number }, data: WorkflowNodeData, opts?: { parentId?: string; width?: number; height?: number }) => void;
		onRemoveNodes?: (ids: string[]) => void;
		onMoveNodes?: (moves: Array<{ id: string; position: { x: number; y: number } }>) => void;
		onAddEdge?: (edge: WorkflowEdge) => void;
		onRemoveEdges?: (ids: string[]) => void;
	};

	let { graph, readonly = false, onchange, onselect, onAddNode, onRemoveNodes, onMoveNodes, onAddEdge, onRemoveEdges }: Props = $props();

	const useGranular = $derived(!!(onAddNode || onRemoveNodes || onMoveNodes || onAddEdge || onRemoveEdges));

	// Track graph identity to avoid re-syncing our own changes
	let lastGraphRef: WorkflowGraph | null = graph;

	let nodes = $state.raw<Node[]>(toFlowNodes(graph));
	let edges = $state.raw<Edge[]>(toFlowEdges(graph));

	function toFlowNodes(g: WorkflowGraph): Node[] {
		// Scope/group nodes must come before their children in the array
		const sorted = [...g.nodes].sort((a, b) => {
			if (a.type === 'scope' && b.type !== 'scope') return -1;
			if (a.type !== 'scope' && b.type === 'scope') return 1;
			return 0;
		});
		return sorted.map((n) => ({
			id: n.id,
			type: n.type,
			position: n.position,
			data: n.data,
			...(n.parentId ? { parentId: n.parentId, extent: 'parent' as const } : {}),
			...(n.width != null ? { width: n.width } : {}),
			...(n.height != null ? { height: n.height } : {})
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
			// Scope nodes must come before their children
			const sorted = [...graph.nodes].sort((a, b) => {
				if (a.type === 'scope' && b.type !== 'scope') return -1;
				if (a.type !== 'scope' && b.type === 'scope') return 1;
				return 0;
			});
			nodes = sorted.map((n) => {
				const existing = currentNodes.get(n.id);
				return {
					id: n.id,
					type: n.type,
					position: existing?.position ?? n.position,
					data: n.data,
					...(n.parentId ? { parentId: n.parentId, extent: 'parent' as const } : {}),
					...(n.width != null ? { width: n.width } : {}),
					...(n.height != null ? { height: n.height } : {}),
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
				data: n.data as WorkflowNodeData,
				...(n.parentId ? { parentId: n.parentId } : {}),
				...(n.width != null ? { width: n.width } : {}),
				...(n.height != null ? { height: n.height } : {})
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
		const edgeId = `e-${connection.source}-${connection.target}-${Date.now()}`;
		const newEdge: Edge = {
			id: edgeId,
			source: connection.source!,
			target: connection.target!,
			sourceHandle: connection.sourceHandle,
			targetHandle: connection.targetHandle,
			type: 'default'
		};
		edges = [...edges, newEdge];

		if (useGranular && onAddEdge) {
			onAddEdge({
				id: edgeId,
				source: connection.source!,
				target: connection.target!,
				sourceHandle: connection.sourceHandle ?? undefined,
				type: 'sequence'
			});
		} else {
			serializeAndEmit();
		}
	}

	function handleSelectionChange({ nodes: selectedNodes }: { nodes: Node[] }) {
		if (selectedNodes.length === 1) {
			onselect?.(selectedNodes[0].id);
		} else {
			onselect?.(null);
		}
	}

	function handleNodeDragStop({ nodes: draggedNodes }: { nodes: Node[] }) {
		if (useGranular && onMoveNodes) {
			onMoveNodes(draggedNodes.map((n) => ({ id: n.id, position: n.position })));
		} else {
			serializeAndEmit();
		}
	}

	function handleDelete({ nodes: deletedNodes, edges: deletedEdges }: { nodes: Node[]; edges: Edge[] }) {
		if (readonly) return;
		const idsToDelete = new Set(deletedNodes.map((n) => n.id));
		// Also delete children of deleted scope nodes
		for (const n of nodes) {
			if (n.parentId && idsToDelete.has(n.parentId)) {
				idsToDelete.add(n.id);
			}
		}

		// Collect edge ids that will be removed (connected to deleted nodes + explicitly deleted edges)
		const edgeIdsToDelete = new Set(deletedEdges.map((e) => e.id));
		for (const e of edges) {
			if (idsToDelete.has(e.source) || idsToDelete.has(e.target)) {
				edgeIdsToDelete.add(e.id);
			}
		}

		nodes = nodes.filter((n) => !idsToDelete.has(n.id));
		edges = edges.filter((e) => !edgeIdsToDelete.has(e.id));

		if (useGranular) {
			if (idsToDelete.size > 0 && onRemoveNodes) {
				onRemoveNodes([...idsToDelete]);
			}
			if (edgeIdsToDelete.size > 0 && onRemoveEdges) {
				// Only report edges not already removed by node deletion
				const standaloneEdgeIds = deletedEdges.map((e) => e.id);
				if (standaloneEdgeIds.length > 0) onRemoveEdges(standaloneEdgeIds);
			}
		} else {
			serializeAndEmit();
		}
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
		const nodeId = `node-${Date.now()}`;
		const data = createDefaultNodeData(nodeType);
		const opts = nodeType === 'scope' ? { width: 400, height: 200 } : undefined;
		const newNode: Node = {
			id: nodeId,
			type: nodeType,
			position,
			data,
			...(opts ?? {})
		};

		// Scope nodes must come before their children
		if (nodeType === 'scope') {
			nodes = [newNode, ...nodes];
		} else {
			nodes = [...nodes, newNode];
		}

		if (useGranular && onAddNode) {
			onAddNode(nodeId, nodeType, position, data, opts);
		} else {
			serializeAndEmit();
		}
	}

	function onDragOver(event: DragEvent) {
		event.preventDefault();
		if (event.dataTransfer) {
			event.dataTransfer.dropEffect = 'move';
		}
	}
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="flex" style="width: 100%; height: 100%;" data-testid="canvas-container">
	{#if !readonly}
		<NodePalette />
	{/if}
	<div style="flex: 1; height: 100%; position: relative;" data-testid="canvas-drop-zone" ondrop={onDrop} ondragover={onDragOver}>
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
			<MiniMap position="bottom-left" data-testid="canvas-minimap" />
		</SvelteFlow>
	</div>
</div>
