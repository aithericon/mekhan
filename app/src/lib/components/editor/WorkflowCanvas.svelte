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
	import { edgeTypes } from './edges';
	import NodePalette from './NodePalette.svelte';
	import DropHandler from './DropHandler.svelte';
	import { compileErrors } from '$lib/editor/compile-errors.svelte';
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
	let edges = $state.raw<Edge[]>(toFlowEdges(graph, readonly));

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
			...(n.height != null ? { height: n.height } : {}),
			...(compileErrors.byNodeId.has(n.id) ? { className: 'workflow-node-error' } : {})
		}));
	}

	function toFlowEdges(g: WorkflowGraph, isReadonly: boolean): Edge[] {
		return g.edges.map((e) => ({
			id: e.id,
			source: e.source,
			target: e.target,
			sourceHandle: e.sourceHandle ?? undefined,
			targetHandle: e.targetHandle ?? undefined,
			label: e.label ?? undefined,
			type: 'deletable' as const,
			animated: e.type === 'loop_back',
			deletable: !isReadonly,
			...(compileErrors.byEdgeId.has(e.id)
				? {
						className: 'workflow-edge-error',
						style: 'stroke: hsl(var(--destructive)); stroke-width: 2;'
					}
				: {})
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
			edges = toFlowEdges(graph, readonly);
		}
	});

	// Re-decorate nodes/edges when the compile-error store changes (e.g. after
	// a failed publish or an explicit clear). We keep all other state (positions,
	// selection, data) intact and just toggle the `className` / `style` fields.
	$effect(() => {
		const nodeErrors = compileErrors.byNodeId;
		const edgeErrors = compileErrors.byEdgeId;
		nodes = nodes.map((n) => {
			const hasErr = nodeErrors.has(n.id);
			const { className: _, ...rest } = n as Node & { className?: string };
			return hasErr ? { ...rest, className: 'workflow-node-error' } : rest;
		});
		edges = edges.map((e) => {
			const hasErr = edgeErrors.has(e.id);
			const { className: _c, style: _s, ...rest } = e as Edge & {
				className?: string;
				style?: string;
			};
			return hasErr
				? {
						...rest,
						className: 'workflow-edge-error',
						style: 'stroke: hsl(var(--destructive)); stroke-width: 2;'
					}
				: rest;
		});
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
				// Phase 2 typed-ports: every edge must carry a target_handle at
				// publish. We default to "in" so legacy edges round-trip cleanly
				// once the user opens the editor; new connections set this via
				// xyflow's Connection (see onConnect).
				targetHandle: (e.targetHandle as string | undefined) ?? 'in',
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
			type: 'deletable'
		};
		edges = [...edges, newEdge];

		if (useGranular && onAddEdge) {
			onAddEdge({
				id: edgeId,
				source: connection.source!,
				target: connection.target!,
				sourceHandle: connection.sourceHandle ?? undefined,
				// Phase 2 hard-require: target_handle must be present on the
				// wire. Fall back to "in" when xyflow returns null (user dropped
				// on the node body without a specific handle).
				targetHandle: connection.targetHandle ?? 'in',
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
			{edgeTypes}
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

<style>
	/* Phase 2 publish-error highlighting: nodes / edges flagged by the
	   compiler get a red ring + outline so the user can see exactly which
	   element triggered the failure. Cleared on the next successful publish. */
	:global(.svelte-flow__node.workflow-node-error) {
		outline: 2px solid hsl(var(--destructive));
		outline-offset: 4px;
		border-radius: 12px;
	}
	:global(.svelte-flow__edge.workflow-edge-error path.svelte-flow__edge-path) {
		stroke: hsl(var(--destructive));
		stroke-width: 2.5;
	}
</style>
