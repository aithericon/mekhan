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
		/**
		 * Emitted when a node's container parent changes (drag-into / drag-out
		 * of a Scope/Loop container). `parentId: null` clears the parent.
		 * `position` is the new **parent-relative** position when entering a
		 * container, or the absolute position when leaving one.
		 */
		onReparentNodes?: (
			changes: Array<{ id: string; parentId: string | null; position?: { x: number; y: number } }>
		) => void;
		onAddEdge?: (edge: WorkflowEdge) => void;
		onRemoveEdges?: (ids: string[]) => void;
	};

	let { graph, readonly = false, onchange, onselect, onAddNode, onRemoveNodes, onMoveNodes, onReparentNodes, onAddEdge, onRemoveEdges }: Props = $props();

	const useGranular = $derived(!!(onAddNode || onRemoveNodes || onMoveNodes || onReparentNodes || onAddEdge || onRemoveEdges));

	// Track graph identity to avoid re-syncing our own changes
	let lastGraphRef: WorkflowGraph | null = graph;

	let nodes = $state.raw<Node[]>(toFlowNodes(graph));
	let edges = $state.raw<Edge[]>(toFlowEdges(graph, readonly));

	// Container kinds — must come before their children in the node array so
	// Svelte Flow can resolve `parentId` on child mount. Currently `scope`
	// (free-form grouping) and `loop` (body authoring); future container
	// kinds (e.g. SubWorkflow inline) get added here.
	function isContainer(t: string | undefined): boolean {
		return t === 'scope' || t === 'loop';
	}
	function containerSort<T extends { type: string }>(a: T, b: T): number {
		if (isContainer(a.type) && !isContainer(b.type)) return -1;
		if (!isContainer(a.type) && isContainer(b.type)) return 1;
		return 0;
	}

	// World position of a node, walking up the parent chain. Used by drop
	// hit-testing AND the drag-stop reparent logic — keep it module-local so
	// both call sites agree on the coordinate system.
	function worldPosOf(n: { position: { x: number; y: number }; parentId?: string }): { x: number; y: number } {
		let x = n.position.x;
		let y = n.position.y;
		let pid = n.parentId;
		while (pid) {
			const p = nodes.find((m) => m.id === pid);
			if (!p) break;
			x += p.position.x;
			y += p.position.y;
			pid = p.parentId;
		}
		return { x, y };
	}

	/**
	 * Find the topmost container node whose bounds contain the given flow
	 * position. Skips the candidate node itself (a container can't parent
	 * itself) and any descendant of the candidate (no recursive parenting).
	 *
	 * Bounds come from `width`/`height` (resizable containers) — if absent,
	 * we don't try to compute them: a container with no explicit size isn't
	 * a valid drop target. World coordinates are computed by walking up the
	 * parent chain so nested containers in the future Just Work.
	 */
	function findContainerAt(
		flowPos: { x: number; y: number },
		skipId?: string
	): Node | null {
		// Build a parent-id → child-ids index once for descendant skipping.
		const childrenOf = new Map<string, Set<string>>();
		for (const n of nodes) {
			if (n.parentId) {
				if (!childrenOf.has(n.parentId)) childrenOf.set(n.parentId, new Set());
				childrenOf.get(n.parentId)!.add(n.id);
			}
		}
		const descendants = new Set<string>();
		if (skipId) {
			const stack = [skipId];
			while (stack.length) {
				const id = stack.pop()!;
				descendants.add(id);
				const kids = childrenOf.get(id);
				if (kids) for (const k of kids) stack.push(k);
			}
		}

		let best: Node | null = null;
		for (const n of nodes) {
			if (!isContainer(n.type)) continue;
			if (descendants.has(n.id)) continue;
			const w = n.width;
			const h = n.height;
			if (w == null || h == null) continue;
			const { x, y } = worldPosOf(n);
			if (flowPos.x >= x && flowPos.x <= x + w && flowPos.y >= y && flowPos.y <= y + h) {
				// Prefer the deepest container at this point (innermost
				// matches win) — naive depth via parent-chain length.
				let depth = 0;
				let pid = n.parentId;
				while (pid) {
					depth += 1;
					const p = nodes.find((m) => m.id === pid);
					if (!p) break;
					pid = p.parentId;
				}
				if (best == null) {
					best = n;
				} else {
					let bestDepth = 0;
					let bpid = best.parentId;
					while (bpid) {
						bestDepth += 1;
						const p = nodes.find((m) => m.id === bpid);
						if (!p) break;
						bpid = p.parentId;
					}
					if (depth > bestDepth) best = n;
				}
			}
		}
		return best;
	}

	function toFlowNodes(g: WorkflowGraph): Node[] {
		const sorted = [...g.nodes].sort(containerSort);
		return sorted.map((n) => ({
			id: n.id,
			type: n.type,
			position: n.position,
			data: n.data,
			// NOTE: no `extent: 'parent'` — that locks a child inside its
			// container's bounds, which kills the drag-OUT gesture. Children
			// still follow their parent on parent-drag because their
			// `position` is parent-relative regardless of `extent`.
			...(n.parentId ? { parentId: n.parentId } : {}),
			...(n.width != null ? { width: n.width } : {}),
			...(n.height != null ? { height: n.height } : {})
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
			deletable: !isReadonly
		}));
	}

	// Re-sync from parent only when graph prop changes externally
	$effect.pre(() => {
		if (graph !== lastGraphRef) {
			lastGraphRef = graph;
			const currentNodes = new Map(nodes.map((n) => [n.id, n]));
			const sorted = [...graph.nodes].sort(containerSort);
			nodes = sorted.map((n) => {
				const existing = currentNodes.get(n.id);
				return {
					id: n.id,
					type: n.type,
					position: existing?.position ?? n.position,
					data: n.data,
					...(n.parentId ? { parentId: n.parentId } : {}),
					...(n.width != null ? { width: n.width } : {}),
					...(n.height != null ? { height: n.height } : {}),
					...(existing?.selected != null ? { selected: existing.selected } : {})
				};
			});
			edges = toFlowEdges(graph, readonly);
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
		// On drag-stop we re-hit-test every dragged non-container node against
		// the container set:
		//   - new container == current parent → pure move (no reparent)
		//   - new container != current parent → reparent (covers drag-IN
		//     from top level, drag-OUT to top level, and drag-across between
		//     containers in a single branch)
		// Containers themselves never auto-reparent today (no nesting UX).
		const reparents: Array<{
			id: string;
			parentId: string | null;
			position?: { x: number; y: number };
		}> = [];
		const moves: Array<{ id: string; position: { x: number; y: number } }> = [];
		for (const n of draggedNodes) {
			if (isContainer(n.type)) {
				moves.push({ id: n.id, position: n.position });
				continue;
			}

			// Compute the dragged node's current world position. xyflow keeps
			// `position` parent-relative for parented children and absolute
			// for top-level nodes, so we resolve through `worldPosOf` either
			// way.
			const childWorld = worldPosOf(n);
			const newContainer = findContainerAt(childWorld, n.id);
			const oldParentId = n.parentId ?? null;
			const newParentId = newContainer?.id ?? null;

			if (newParentId === oldParentId) {
				moves.push({ id: n.id, position: n.position });
				continue;
			}

			// Reparent. Position becomes relative when entering a container,
			// absolute (world) when leaving to top level.
			let newPosition: { x: number; y: number };
			if (newContainer) {
				const containerWorld = worldPosOf(newContainer);
				newPosition = {
					x: childWorld.x - containerWorld.x,
					y: childWorld.y - containerWorld.y
				};
			} else {
				newPosition = childWorld;
			}

			nodes = nodes.map((m) => {
				if (m.id !== n.id) return m;
				// Rebuild without parentId so it's truly cleared on drag-OUT;
				// a `parentId: undefined` spread won't drop the key.
				const { parentId: _p, ...rest } = m;
				return newParentId
					? { ...rest, parentId: newParentId, position: newPosition }
					: { ...rest, position: newPosition };
			});
			reparents.push({ id: n.id, parentId: newParentId, position: newPosition });
		}

		if (useGranular) {
			if (moves.length && onMoveNodes) onMoveNodes(moves);
			if (reparents.length && onReparentNodes) onReparentNodes(reparents);
			// If reparents fired but the consumer didn't wire onReparentNodes,
			// fall through to the bulk serializer so Yjs still observes the
			// state change.
			if (reparents.length && !onReparentNodes) serializeAndEmit();
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

		const dropPos = screenToFlowPos({ x: event.clientX, y: event.clientY });
		const nodeId = `node-${Date.now()}`;
		const data = createDefaultNodeData(nodeType);

		// Container kinds get a default initial size so they're a valid drop
		// target for future drag-into operations. Scope and Loop both behave
		// as resizable containers.
		const sizeOpts: { width?: number; height?: number } =
			isContainer(nodeType) ? { width: 400, height: 200 } : {};

		// If dropped inside a container (and the new node is not itself a
		// container), parent it. A container dropped inside another container
		// is a future case (nesting); not supported in the UX today.
		const container = !isContainer(nodeType) ? findContainerAt(dropPos) : null;
		let position = dropPos;
		let parentId: string | undefined;
		if (container) {
			const containerWorld = (() => {
				let x = container.position.x;
				let y = container.position.y;
				let pid = container.parentId;
				while (pid) {
					const p = nodes.find((m) => m.id === pid);
					if (!p) break;
					x += p.position.x;
					y += p.position.y;
					pid = p.parentId;
				}
				return { x, y };
			})();
			position = { x: dropPos.x - containerWorld.x, y: dropPos.y - containerWorld.y };
			parentId = container.id;
		}

		const opts: { parentId?: string; width?: number; height?: number } = {
			...sizeOpts,
			...(parentId ? { parentId } : {})
		};
		const optsForEmit = Object.keys(opts).length ? opts : undefined;

		const newNode: Node = {
			id: nodeId,
			type: nodeType,
			position,
			data,
			...(parentId ? { parentId } : {}),
			...sizeOpts
		};

		// Containers must come before their children in the array (Svelte
		// Flow resolves parentId on mount in order).
		if (isContainer(nodeType)) {
			nodes = [newNode, ...nodes];
		} else {
			nodes = [...nodes, newNode];
		}

		if (useGranular && onAddNode) {
			onAddNode(nodeId, nodeType, position, data, optsForEmit);
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

