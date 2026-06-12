<script lang="ts">
	import {
		SvelteFlow,
		Controls,
		Background,
		MiniMap,
		Panel,
		BackgroundVariant,
		type Node,
		type Edge,
		type Connection
	} from '@xyflow/svelte';
	import '@xyflow/svelte/dist/style.css';
	import { mode } from 'mode-watcher';
	import LayoutGrid from '@lucide/svelte/icons/layout-grid';

	import { setContext } from 'svelte';
	import { nodeTypes } from './nodes';
	import { edgeTypes } from './edges';
	import NodePalette from './NodePalette.svelte';
	import DropHandler from './DropHandler.svelte';
	import { RESIZE_REPORT_CONTEXT_KEY, type ResizeReport } from './nodes/resize-context';
	import { provideEdgeJoin, type SetEdgeJoin } from './edges/edge-join-context';
	import { layoutWorkflowGraph } from '$lib/editor/workflow-layout';
	import {
		createDefaultNodeData,
		type WorkflowNodeData,
		type WorkflowNodeType,
		type WorkflowEdge,
		type WorkflowGraph
	} from '$lib/types/editor';
	import type { XYPosition } from '@xyflow/svelte';
	import { edgeLaneColor, channelForSourceHandle } from '$lib/editor/edge-lane';

	type Props = {
		graph: WorkflowGraph;
		readonly?: boolean;
		onchange?: (graph: WorkflowGraph) => void;
		/**
		 * Fires whenever xyflow's selection set changes — including reactive
		 * re-emits triggered by `store.nodes` being reassigned (e.g. when
		 * `updateNodeInternals` updates dimensions during a runtime data
		 * refresh). Suitable for the template editor's "selected node →
		 * property panel" flow where keyboard selection should also count.
		 * For "user clicked a node, open a drawer" flows, prefer
		 * `onNodeClick` + `onPaneClick` — those fire only on real pointer
		 * events and won't be re-emitted by dimension churn.
		 */
		onselect?: (nodeId: string | null) => void;
		/** User-click on a node. Distinct from `onselect` — see above. */
		onNodeClick?: (nodeId: string) => void;
		/** User-click on the empty pane (i.e. outside any node/edge). */
		onPaneClick?: () => void;
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
		/**
		 * Emitted when an existing edge's own properties change (today: the
		 * channel join discipline toggled via the on-edge chip). The patch is
		 * sparse — only the touched keys are present; `join: null` means
		 * "restore the 'each' default" (the persisted key is deleted).
		 */
		onUpdateEdge?: (edgeId: string, patch: { join?: 'gather' | null }) => void;
		/**
		 * Emitted when a resizable container node (Scope, Loop) finishes a
		 * resize gesture. `position` is only set when the gesture moved the
		 * node's top-left corner (top/left-edge resize); pure bottom-right
		 * resizes omit it.
		 */
		onResizeNodes?: (
			changes: Array<{
				id: string;
				width: number;
				height: number;
				position?: { x: number; y: number };
			}>
		) => void;
	};

	let { graph, readonly = false, onchange, onselect, onNodeClick, onPaneClick, onAddNode, onRemoveNodes, onMoveNodes, onReparentNodes, onAddEdge, onRemoveEdges, onUpdateEdge, onResizeNodes }: Props = $props();

	const useGranular = $derived(!!(onAddNode || onRemoveNodes || onMoveNodes || onReparentNodes || onAddEdge || onRemoveEdges || onUpdateEdge || onResizeNodes));

	// Container nodes report resize gesture-end through this context. xyflow's
	// NodeResizer has already mutated the bound `nodes` array with the new
	// dims/pos by the time `onResizeEnd` fires, so the canvas just forwards
	// the change to the granular sink or runs the bulk serializer.
	//
	// Context is registered only when editable. On readonly canvases the
	// children see `undefined` and `NodeResizer.isVisible` collapses to false,
	// so the handles never render — without this gate the resizer would draw,
	// xyflow would mutate the local nodes array on drag, but persistence
	// would silently no-op, leaving the user with a phantom resize.
	// svelte-ignore state_referenced_locally
	if (!readonly) {
		const reportResize: ResizeReport = (id, params) => {
			const change = {
				id,
				width: params.width,
				height: params.height,
				// Position only travels with the change when xyflow moved it
				// (top/left-edge resize). For bottom-right resizes x/y match the
				// node's pre-gesture position, but we still pass them through
				// since the binding writes `position` only when present and the
				// extra write is a no-op against an unchanged value.
				position: { x: params.x, y: params.y }
			};
			if (useGranular && onResizeNodes) {
				onResizeNodes([change]);
			} else {
				serializeAndEmit();
			}
		};
		setContext<ResizeReport>(RESIZE_REPORT_CONTEXT_KEY, reportResize);

		// Control-channel join chip (DeletableEdge) toggles each ⇄ gather through
		// this context. Update the local edge data first (the chip re-renders
		// immediately), then persist through the granular sink or the bulk
		// serializer. `join: null` restores the 'each' default.
		const setEdgeJoin: SetEdgeJoin = (edgeId, join) => {
			edges = edges.map((e) =>
				e.id === edgeId ? { ...e, data: { ...e.data, join: join ?? undefined } } : e
			);
			if (useGranular && onUpdateEdge) {
				onUpdateEdge(edgeId, { join });
			} else {
				serializeAndEmit();
			}
		};
		provideEdgeJoin(setEdgeJoin);
	}

	// Track graph identity to avoid re-syncing our own changes
	// svelte-ignore state_referenced_locally
	let lastGraphRef: WorkflowGraph | null = graph;

	// Local canvas state is seeded from the initial graph/readonly and then
	// owned here; the {@const}/lastGraphRef sync below re-applies upstream
	// graph swaps, so the initial-value capture is deliberate.
	// svelte-ignore state_referenced_locally
	let nodes = $state.raw<Node[]>(toFlowNodes(graph));
	// svelte-ignore state_referenced_locally
	let edges = $state.raw<Edge[]>(toFlowEdges(graph, readonly));

	// Container kinds — must come before their children in the node array so
	// Svelte Flow can resolve `parentId` on child mount. `scope` is free-form
	// grouping, `loop` and `timeout` are body authoring (the wrapped child
	// nodes set `parentId == container.id`). Future container kinds (e.g.
	// SubWorkflow inline) get added here.
	function isContainer(t: string | undefined): boolean {
		return t === 'scope' || t === 'lease_scope' || t === 'loop' || t === 'timeout' || t === 'map';
	}

	/**
	 * Order nodes so every parent precedes its descendants. xyflow derives a
	 * child's absolute position by adding its parent's, which REQUIRES the parent
	 * to come first in the nodes array — otherwise the child renders at its raw
	 * parent-relative coordinates (the parent offset is silently dropped), which
	 * looks like the child escaping its container.
	 *
	 * The stored graph order is arbitrary w.r.t. nesting — a nested container
	 * (e.g. a loop INSIDE a lease scope) routinely appears before its parent. A
	 * flat "containers first" partition does NOT fix this: it keeps two
	 * sibling-ordered containers in their (wrong) input order, so the inner
	 * container can still land ahead of its outer parent. Sorting by nesting
	 * depth (ancestor count) is the robust fix — depth 0 (top level) first, then
	 * each level — and a stable sort preserves sibling order within a level.
	 */
	function orderByNesting<T extends { id: string; parentId?: string | null }>(list: T[]): T[] {
		const byId = new Map(list.map((n) => [n.id, n]));
		const depthOf = (n: T): number => {
			let d = 0;
			let pid = n.parentId;
			const seen = new Set<string>();
			while (pid && byId.has(pid) && !seen.has(pid)) {
				seen.add(pid);
				d += 1;
				pid = byId.get(pid)!.parentId;
			}
			return d;
		};
		return [...list].sort((a, b) => depthOf(a) - depthOf(b));
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
		const sorted = orderByNesting(g.nodes);
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
		const byId = new Map(g.nodes.map((n) => [n.id, n]));
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
			// Tint the edge to match the ports it connects so it reads as a lane:
			// a gradient from the source port's color to the target port's color.
			// `channelPlane` (which declared Channel the edge taps, if any) and
			// `join` (the consumer-side discipline) feed the on-edge join chip;
			// lane colors are view-only, `join` round-trips via serializeAndEmit.
			data: {
				laneFrom: edgeLaneColor(byId.get(e.source), e.sourceHandle),
				laneTo: edgeLaneColor(byId.get(e.target), e.targetHandle),
				channelPlane: channelForSourceHandle(byId.get(e.source), e.sourceHandle)?.plane,
				join: e.join ?? undefined
			}
		}));
	}

	// Re-sync from parent only when graph prop changes externally
	$effect.pre(() => {
		if (graph !== lastGraphRef) {
			lastGraphRef = graph;
			const currentNodes = new Map(nodes.map((n) => [n.id, n]));
			const sorted = orderByNesting(graph.nodes);
			nodes = sorted.map((n) => {
				const existing = currentNodes.get(n.id);
				return {
					id: n.id,
					type: n.type,
					// The graph (Yjs) position is authoritative — taking it here is
					// what makes undo/redo and co-author moves actually render. The
					// one exception is a node mid-drag: xyflow owns its position for
					// the duration of the gesture, and snapping it back to the (stale)
					// graph value would yank it out of the user's hand. Drag-stop
					// writes the final position to Yjs, which round-trips right back
					// through this sync.
					position: existing?.dragging ? existing.position : n.position,
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
				type: e.animated ? ('loop_back' as const) : ('sequence' as const),
				// Channel join discipline: only the non-default 'gather' is
				// serialized — an absent key IS the 'each' default, so legacy
				// edges round-trip byte-stable.
				...(e.data?.join === 'gather' ? { join: 'gather' as const } : {})
			}))
		};
		lastGraphRef = serialized;
		onchange?.(serialized);
	}

	function onConnect(connection: Connection) {
		if (readonly) return;
		const edgeId = `e-${connection.source}-${connection.target}-${Date.now()}`;
		// Tools-handle source → agent-binding edge (not a sequence arc):
		// the compiler discovers tools via these edges and mints the
		// dispatch/collect transitions itself. Stamp the on-wire `type`
		// accordingly so the engine + visualisation can render it
		// distinctly from a regular data flow.
		const isToolsEdge = connection.sourceHandle === 'tools';
		// An edge dropped on a Loop/Timeout `body_out` handle is the body
		// return arc — it closes the container's body cycle, so it must be
		// stamped `loop_back` (excluded from the compiler DAG + rendered
		// animated). Without this the edge persists as a plain `sequence`
		// and the cycle detector rejects the graph ("cycle in non-loop edges").
		const isBodyReturn = connection.targetHandle === 'body_out';
		const newEdge: Edge = {
			id: edgeId,
			source: connection.source!,
			target: connection.target!,
			sourceHandle: connection.sourceHandle,
			targetHandle: connection.targetHandle,
			type: 'deletable',
			animated: isBodyReturn,
			data: {
				laneFrom: edgeLaneColor(
					nodes.find((n) => n.id === connection.source),
					connection.sourceHandle
				),
				laneTo: edgeLaneColor(
					nodes.find((n) => n.id === connection.target),
					connection.targetHandle
				),
				// A freshly drawn control-channel edge gets its join chip
				// immediately (join is unset ⇒ the 'each' default).
				channelPlane: channelForSourceHandle(
					nodes.find((n) => n.id === connection.source),
					connection.sourceHandle
				)?.plane
			}
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
				type: isToolsEdge ? 'tools' : isBodyReturn ? 'loop_back' : 'sequence'
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

	/**
	 * Auto-arrange: recompute every node's position with the dimension-aware
	 * dagre layout (`workflow-layout.ts`), reserving each card at its real
	 * footprint so nodes never overlap. Container nodes are also resized to fit
	 * their children. Persists through the same granular sinks (`onMoveNodes` /
	 * `onResizeNodes`) the drag/resize gestures use, or the bulk serializer.
	 */
	function autoArrange() {
		if (readonly || nodes.length === 0) return;

		const layoutNodes = nodes.map((n) => ({
			id: n.id,
			type: n.type as string,
			data: n.data as WorkflowNodeData,
			parentId: n.parentId,
			width: n.width,
			height: n.height,
			// Real painted footprint (xyflow measures mounted nodes into `measured`)
			// so the layout reserves exactly what's on screen — see workflow-layout.
			measuredWidth: n.measured?.width,
			measuredHeight: n.measured?.height
		}));
		const layoutEdges = edges.map((e) => ({
			source: e.source,
			target: e.target,
			sourceHandle: e.sourceHandle as string | undefined,
			targetHandle: e.targetHandle as string | undefined,
			animated: e.animated
		}));

		const { positions, containerSizes } = layoutWorkflowGraph(layoutNodes, layoutEdges);

		nodes = nodes.map((n) => {
			const pos = positions.get(n.id);
			const size = containerSizes.get(n.id);
			const next: Node = { ...n };
			if (pos) next.position = pos;
			if (size) {
				next.width = size.width;
				next.height = size.height;
			}
			return next;
		});

		if (useGranular) {
			const moves: Array<{ id: string; position: { x: number; y: number } }> = [];
			const resizes: Array<{
				id: string;
				width: number;
				height: number;
				position?: { x: number; y: number };
			}> = [];
			for (const n of nodes) {
				const pos = positions.get(n.id);
				const size = containerSizes.get(n.id);
				if (size) {
					resizes.push({ id: n.id, width: size.width, height: size.height, position: pos });
				} else if (pos) {
					moves.push({ id: n.id, position: pos });
				}
			}
			let handled = false;
			if (moves.length && onMoveNodes) {
				onMoveNodes(moves);
				handled = true;
			}
			if (resizes.length && onResizeNodes) {
				onResizeNodes(resizes);
				handled = true;
			}
			// If any change couldn't be routed through a granular sink, fall back
			// to the bulk serializer so the layout still persists.
			if (!handled || (moves.length && !onMoveNodes) || (resizes.length && !onResizeNodes)) {
				serializeAndEmit();
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
			colorMode={mode.current ?? 'system'}
			onconnect={onConnect}
			onnodeclick={onNodeClick ? ({ node }) => onNodeClick!(node.id) : undefined}
			onpaneclick={onPaneClick ? () => onPaneClick!() : undefined}
			onselectionchange={handleSelectionChange}
			ondelete={handleDelete}
			onnodedragstop={handleNodeDragStop}
			fitView
			deleteKey={["Backspace", "Delete"]}
			proOptions={{ hideAttribution: true }}
		>
			<DropHandler oninit={handleFlowInit} />
			{#if !readonly}
				<Panel position="top-right">
					<button
						type="button"
						onclick={autoArrange}
						title="Auto-arrange — lay the graph out left-to-right, sized to each node"
						data-testid="canvas-auto-arrange"
						class="flex items-center gap-1.5 rounded-md border border-border bg-card px-2.5 py-1.5 text-sm font-medium text-foreground shadow-sm transition-colors hover:bg-muted"
					>
						<LayoutGrid class="size-3.5" />
						Auto-arrange
					</button>
				</Panel>
			{/if}
			<Controls position="bottom-right" />
			<Background variant={BackgroundVariant.Dots} gap={36} size={2.5} />
			<MiniMap position="bottom-left" data-testid="canvas-minimap" />
		</SvelteFlow>
	</div>
</div>

