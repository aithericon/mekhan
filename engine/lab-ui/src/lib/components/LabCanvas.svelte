<script lang="ts">
	import dagre from '@dagrejs/dagre';
	import {
		SvelteFlow,
		Background,
		Controls,
		MiniMap,
		Position,
		MarkerType,
		type Node,
		type Edge
	} from '@xyflow/svelte';
	import '@xyflow/svelte/dist/style.css';
	import { mode } from 'mode-watcher';
	import * as Tooltip from '$lib/components/ui/tooltip';
	import type { PetriNet, Token, TransitionStatus } from '$lib/api/client';
	import type { IssueLevel, ValidationIssue, EventSpotlight, MarkingDiff } from '$lib/stores/lab.svelte';
	import PlaceNode from './PlaceNode.svelte';
	import TransitionNode from './TransitionNode.svelte';
	import GroupNode from './GroupNode.svelte';
	import MetaGroupNode from './MetaGroupNode.svelte';
	import RemoteNetNode from './RemoteNetNode.svelte';
	import CanvasController from './CanvasController.svelte';
	import NodeSearch from './NodeSearch.svelte';

	// Group type from scenario
	export interface ScenarioGroup {
		id: string;
		name: string;
		parent_id?: string;
		metadata?: Record<string, unknown>;
	}

	interface Props {
		presentationMode?: boolean;
		topology: PetriNet | null;
		marking: Map<string, Token[]>;
		bridgedOutTokens?: Map<string, Token[]>;
		enabledTransitions: string[];
		transitionStatuses: Record<string, TransitionStatus>;
		/** Current net ID (for spawn child lookup). */
		netId?: string;
		/** Spawn children grouped by parent net ID. */
		spawnChildren?: Map<string, { netId: string; label: string }[]>;
		/** Navigate to a child net's tab. */
		onNavigateToChild?: (netId: string) => void;
		issues?: ValidationIssue[];
		groups?: ScenarioGroup[];
		selectedElementId?: string | null;
		spotlight?: EventSpotlight | null;
		markingDiff?: MarkingDiff | null;
		onFireTransition: (transitionId: string) => void;
		onSelectPlace?: (placeId: string) => void;
		onSelectTransition?: (transitionId: string) => void;
		onSelectToken?: (placeId: string, tokenId: string) => void;
		onSelectGroup?: (groupId: string) => void;
		onSelectRemoteNet?: (id: string, label: string, targets: string[], sources: string[], childNetIds: string[]) => void;
	}

	let { presentationMode = false, topology, marking, bridgedOutTokens, enabledTransitions, transitionStatuses, issues = [], groups = [], selectedElementId = null, spotlight = null, markingDiff = null, netId, spawnChildren, onNavigateToChild, onFireTransition, onSelectPlace, onSelectTransition, onSelectToken, onSelectGroup, onSelectRemoteNet }: Props = $props();

	let showCausation = $state(true);
	let showBridges = $state(true);
	let showReadArcs = $state(true);
	let collapseGroups = $state(false);

	const colorMode = $derived(mode.current === 'light' ? 'light' as const : 'dark' as const);

	// Severity ordering for keeping highest severity
	const severityOrder: Record<IssueLevel, number> = { error: 0, warning: 1, info: 2 };

	// Build a map from node ID to highest severity issue level
	const issuesMap = $derived.by(() => {
		const map = new Map<string, IssueLevel>();
		for (const issue of issues) {
			const existing = map.get(issue.node_id);
			// Keep highest severity (lower number = higher severity)
			if (!existing || severityOrder[issue.level] < severityOrder[existing]) {
				map.set(issue.node_id, issue.level);
			}
		}
		return map;
	});

	const nodeTypes = {
		place: PlaceNode,
		transition: TransitionNode,
		group: GroupNode,
		metagroup: MetaGroupNode,
		remotenet: RemoteNetNode
	};

	// SvelteFlow uses node.id as the key in keyed {#each} blocks.
	// Places and transitions live in separate namespaces in a Petri net,
	// so the same ID can appear for both (e.g. "dead_letter" as a place
	// and a transition). Prefix IDs by type to guarantee uniqueness.
	const placeNodeId = (id: string) => `p:${id}`;
	const transNodeId = (id: string) => `t:${id}`;
	const rawId = (prefixed: string) => prefixed.replace(/^[pt]:/, '');

	// Node dimensions for dagre (must match actual component sizes)
	const PLACE_WIDTH = 70;
	const PLACE_HEIGHT = 70;
	const TRANSITION_WIDTH = 200; // Fixed width for transitions
	const GROUP_MIN_WIDTH = 180;
	const GROUP_MIN_HEIGHT = 100;

	// Compute transition height based on port count (matches TransitionNode.svelte layout)
	const TRANS_HEADER_H = 24;     // px-2 py-1 + text
	const TRANS_PORTS_PAD = 9;     // border-t (1px) + py-1 (4px*2)
	const TRANS_PORT_ROW = 14;     // .port-row height
	const TRANS_CAUSE_DIV = 6;     // causation divider (my-0.5 + border)

	function getTransitionHeight(inputCount: number, outputCount: number, causedCount: number): number {
		const rightRows = outputCount + (causedCount > 0 ? causedCount + 1 : 0); // +1 for divider row
		const maxRows = Math.max(inputCount, rightRows, 1);
		return TRANS_HEADER_H + TRANS_PORTS_PAD + maxRows * TRANS_PORT_ROW;
	}

	// Meta-group node dimensions
	const META_WIDTH = 220;
	const META_HEADER_H = 28;
	const META_PORT_ROW = 16;
	const META_PAD = 12;

	function getMetaHeight(inputCount: number, outputCount: number): number {
		const maxPorts = Math.max(inputCount, outputCount, 1);
		return META_HEADER_H + META_PAD + maxPorts * META_PORT_ROW;
	}

	// Get node dimensions by type (fallback for non-transition nodes)
	function getNodeDimensions(type: string): { width: number; height: number } {
		switch (type) {
			case 'place': return { width: PLACE_WIDTH, height: PLACE_HEIGHT };
			case 'transition': return { width: TRANSITION_WIDTH, height: getTransitionHeight(1, 1, 0) };
			case 'group': return { width: GROUP_MIN_WIDTH, height: GROUP_MIN_HEIGHT };
			case 'metagroup': return { width: META_WIDTH, height: getMetaHeight(1, 1) };
			case 'remotenet': return { width: 140, height: 56 };
			default: return { width: 100, height: 100 };
		}
	}

	// Get display label for a node by its prefixed ID
	function getNodeLabel(prefixedId: string, nodes: Node[]): string {
		const node = nodes.find(n => n.id === prefixedId);
		return (node?.data as any)?.label ?? rawId(prefixedId);
	}

	// Collapse groups into meta-nodes with boundary ports
	function applyGroupCollapse(
		rawNodes: Node[],
		rawEdges: Edge[],
		groupDefs: ScenarioGroup[],
		selectedId: string | null | undefined,
		onSelect: ((groupId: string) => void) | undefined,
		spawnInfo?: { childrenByParent: Map<string, { netId: string; label: string }[]>; parentNetId: string; onNavigateToChild?: (netId: string) => void }
	): { nodes: Node[]; edges: Edge[]; groups: ScenarioGroup[] } {
		const rootGroups = groupDefs.filter(g => !g.parent_id);
		if (rootGroups.length === 0) return { nodes: rawNodes, edges: rawEdges, groups: groupDefs };

		// Build descendant group sets for each root group
		const descendantGroups = new Map<string, Set<string>>();
		for (const root of rootGroups) {
			const desc = new Set<string>([root.id]);
			const queue = [root.id];
			while (queue.length > 0) {
				const gid = queue.shift()!;
				for (const g of groupDefs) {
					if (g.parent_id === gid && !desc.has(g.id)) {
						desc.add(g.id);
						queue.push(g.id);
					}
				}
			}
			descendantGroups.set(root.id, desc);
		}

		// Build member sets (node IDs inside each root group)
		const memberSets = new Map<string, Set<string>>();
		const allMembers = new Set<string>();
		for (const [rootId, desc] of descendantGroups) {
			const members = new Set<string>();
			for (const node of rawNodes) {
				if (node.parentId && desc.has(node.parentId)) {
					members.add(node.id);
					allMembers.add(node.id);
				}
			}
			memberSets.set(rootId, members);
		}

		// Track which edges are fully internal (to be removed)
		const internalEdgeIds = new Set<string>();
		// Track edges between two different collapsed groups (both ends get re-routed)
		const metaNodes: Node[] = [];

		// Find which root group a member node belongs to
		function findRootGroup(nodeId: string): string | undefined {
			for (const [rootId, members] of memberSets) {
				if (members.has(nodeId)) return rootId;
			}
			return undefined;
		}

		// First pass: classify edges and collect boundary info per root group
		const inputPortsMap = new Map<string, Map<string, string>>(); // rootId → (externalNodeId → label)
		const outputPortsMap = new Map<string, Map<string, string>>();
		for (const root of rootGroups) {
			inputPortsMap.set(root.id, new Map());
			outputPortsMap.set(root.id, new Map());
		}

		for (const edge of rawEdges) {
			const srcRoot = findRootGroup(edge.source);
			const tgtRoot = findRootGroup(edge.target);

			if (srcRoot && tgtRoot && srcRoot === tgtRoot) {
				// Fully internal to one group
				internalEdgeIds.add(edge.id);
			} else if (srcRoot && tgtRoot && srcRoot !== tgtRoot) {
				// Edge between two collapsed groups — re-route both ends
				// Source side: output port on srcRoot meta-node
				const metaTgtId = `meta-${tgtRoot}`;
				outputPortsMap.get(srcRoot)!.set(metaTgtId, rootGroups.find(g => g.id === tgtRoot)!.name);
				edge.source = `meta-${srcRoot}`;
				edge.sourceHandle = `out-${metaTgtId}`;
				// Target side: input port on tgtRoot meta-node
				const metaSrcId = `meta-${srcRoot}`;
				inputPortsMap.get(tgtRoot)!.set(metaSrcId, rootGroups.find(g => g.id === srcRoot)!.name);
				edge.target = `meta-${tgtRoot}`;
				edge.targetHandle = `in-${metaSrcId}`;
			} else if (!srcRoot && tgtRoot) {
				// External → inside group (input boundary)
				inputPortsMap.get(tgtRoot)!.set(edge.source, getNodeLabel(edge.source, rawNodes));
				edge.target = `meta-${tgtRoot}`;
				edge.targetHandle = `in-${edge.source}`;
			} else if (srcRoot && !tgtRoot) {
				// Inside group → external (output boundary)
				outputPortsMap.get(srcRoot)!.set(edge.target, getNodeLabel(edge.target, rawNodes));
				edge.source = `meta-${srcRoot}`;
				edge.sourceHandle = `out-${edge.target}`;
			}
		}

		// Create meta-nodes
		for (const root of rootGroups) {
			const members = memberSets.get(root.id)!;

			// Count internal elements
			let placeCount = 0, transitionCount = 0, tokenCount = 0;
			for (const nodeId of members) {
				const node = rawNodes.find(n => n.id === nodeId);
				if (!node) continue;
				if (node.type === 'place') {
					placeCount++;
					tokenCount += ((node.data as any).tokens?.length ?? 0)
						+ ((node.data as any).bridgedOutTokens?.length ?? 0);
				} else if (node.type === 'transition') {
					transitionCount++;
				}
			}

			const inputPorts = [...inputPortsMap.get(root.id)!].map(([id, label]) => ({ id: `in-${id}`, label }));
			const outputPorts = [...outputPortsMap.get(root.id)!].map(([id, label]) => ({ id: `out-${id}`, label }));
			const metaHeight = getMetaHeight(inputPorts.length, outputPorts.length);

			// Detect spawn groups via metadata
			const isSpawn = root.metadata?.spawn === true;
			const childNets = isSpawn && spawnInfo
				? (spawnInfo.childrenByParent.get(spawnInfo.parentNetId) ?? [])
				: [];

			metaNodes.push({
				id: `meta-${root.id}`,
				type: 'metagroup',
				position: { x: 0, y: 0 },
				data: {
					label: root.name,
					placeCount,
					transitionCount,
					tokenCount,
					inputPorts,
					outputPorts,
					selected: selectedId === root.id,
					onSelect: () => onSelect?.(root.id),
					isSpawn,
					spawnInstanceCount: isSpawn ? childNets.length : undefined,
					spawnChildNetIds: isSpawn ? childNets.map(c => c.netId) : undefined,
					onNavigateToChild: isSpawn ? spawnInfo?.onNavigateToChild : undefined,
					_dims: { width: META_WIDTH, height: metaHeight }
				}
			});
		}

		// Filter out internal nodes and edges
		const filteredNodes = rawNodes.filter(n => !allMembers.has(n.id));
		const filteredEdges = rawEdges.filter(e => !internalEdgeIds.has(e.id));

		return {
			nodes: [...filteredNodes, ...metaNodes],
			edges: filteredEdges,
			groups: [] // No group boxes when collapsed
		};
	}

	// Layout nodes and edges using dagre with compound graph support
	function getLayoutedElements(
		nodes: Node[],
		edges: Edge[],
		groupNodes: ScenarioGroup[],
		spotlightActive: boolean = false
	): { nodes: Node[]; edges: Edge[] } {
		// Always create compound graph — nodes may have parentId even without explicit groups
		const dagreGraph = new dagre.graphlib.Graph({ compound: true });
		dagreGraph.setDefaultEdgeLabel(() => ({}));
		dagreGraph.setGraph({ rankdir: 'LR', nodesep: 4, ranksep: 12 });

		// Compute nesting depth for each group
		const groupDepthMap = new Map<string, number>();
		const computeDepth = (group: ScenarioGroup): number => {
			if (groupDepthMap.has(group.id)) return groupDepthMap.get(group.id)!;
			const depth = group.parent_id
				? computeDepth(groupNodes.find(g => g.id === group.parent_id)!) + 1
				: 0;
			groupDepthMap.set(group.id, depth);
			return depth;
		};
		groupNodes.forEach(computeDepth);

		// Group padding constants
		const GROUP_PAD_LEFT = 12;
		const GROUP_PAD_RIGHT = 12;
		const GROUP_PAD_TOP = 22;
		const GROUP_PAD_BOTTOM = 8;

		// First, add group nodes with padding so children don't crowd edges
		groupNodes.forEach((group) => {
			dagreGraph.setNode(group.id, {
				width: GROUP_MIN_WIDTH,
				height: GROUP_MIN_HEIGHT,
				paddingLeft: GROUP_PAD_LEFT,
				paddingRight: GROUP_PAD_RIGHT,
				paddingTop: GROUP_PAD_TOP,
				paddingBottom: GROUP_PAD_BOTTOM
			});
			// Set parent for nested groups
			if (group.parent_id) {
				dagreGraph.setParent(group.id, group.parent_id);
			}
		});

		// Add regular nodes to dagre with their dimensions
		nodes.forEach((node) => {
			const dims = (node.data as any)?._dims ?? getNodeDimensions(node.type ?? 'place');
			dagreGraph.setNode(node.id, dims);

			// Set parent for nodes that belong to a group
			if (node.parentId) {
				dagreGraph.setParent(node.id, node.parentId);
			}
		});

		// Add edges to dagre
		edges.forEach((edge) => {
			dagreGraph.setEdge(edge.source, edge.target);
		});

		// Run the layout
		dagre.layout(dagreGraph);

		// Recompute group bounds from actual child positions (dagre compound
		// layout can mis-size groups). Process leaf groups first, then parents.
		const groupsByDepth = [...groupNodes].sort(
			(a, b) => (groupDepthMap.get(b.id) ?? 0) - (groupDepthMap.get(a.id) ?? 0)
		);

		// Collect absolute top-left + dimensions for every node (children + subgroups)
		const absRect = new Map<string, { x: number; y: number; w: number; h: number }>();

		// Seed regular nodes
		nodes.forEach((node) => {
			const pos = dagreGraph.node(node.id);
			if (!pos) return;
			const dims = (node.data as any)?._dims ?? getNodeDimensions(node.type ?? 'place');
			absRect.set(node.id, {
				x: pos.x - dims.width / 2,
				y: pos.y - dims.height / 2,
				w: dims.width,
				h: dims.height
			});
		});

		// Build child-of-group map
		const childrenOf = new Map<string, string[]>();
		groupNodes.forEach((g) => childrenOf.set(g.id, []));
		nodes.forEach((node) => {
			if (node.parentId && childrenOf.has(node.parentId)) {
				childrenOf.get(node.parentId)!.push(node.id);
			}
		});
		// Nested groups are also children of their parent group
		groupNodes.forEach((g) => {
			if (g.parent_id && childrenOf.has(g.parent_id)) {
				childrenOf.get(g.parent_id)!.push(g.id);
			}
		});

		// Compute group bounds bottom-up (deepest first)
		const groupPositions = new Map<string, { x: number; y: number; width: number; height: number }>();
		groupsByDepth.forEach((group) => {
			const kids = childrenOf.get(group.id) ?? [];
			if (kids.length === 0) {
				// No children — use dagre's position
				const pos = dagreGraph.node(group.id);
				if (pos) {
					const w = pos.width ?? GROUP_MIN_WIDTH;
					const h = pos.height ?? GROUP_MIN_HEIGHT;
					groupPositions.set(group.id, { x: pos.x - w / 2, y: pos.y - h / 2, width: w, height: h });
					absRect.set(group.id, { x: pos.x - w / 2, y: pos.y - h / 2, w, h });
				}
				return;
			}

			// Bounding box of all children
			let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
			kids.forEach((kid) => {
				const r = absRect.get(kid);
				if (!r) return;
				minX = Math.min(minX, r.x);
				minY = Math.min(minY, r.y);
				maxX = Math.max(maxX, r.x + r.w);
				maxY = Math.max(maxY, r.y + r.h);
			});

			if (!isFinite(minX)) {
				// Fallback to dagre position
				const pos = dagreGraph.node(group.id);
				if (pos) {
					const w = pos.width ?? GROUP_MIN_WIDTH;
					const h = pos.height ?? GROUP_MIN_HEIGHT;
					groupPositions.set(group.id, { x: pos.x - w / 2, y: pos.y - h / 2, width: w, height: h });
					absRect.set(group.id, { x: pos.x - w / 2, y: pos.y - h / 2, w, h });
				}
				return;
			}

			const gx = minX - GROUP_PAD_LEFT;
			const gy = minY - GROUP_PAD_TOP;
			const gw = Math.max(GROUP_MIN_WIDTH, (maxX - minX) + GROUP_PAD_LEFT + GROUP_PAD_RIGHT);
			const gh = Math.max(GROUP_MIN_HEIGHT, (maxY - minY) + GROUP_PAD_TOP + GROUP_PAD_BOTTOM);

			groupPositions.set(group.id, { x: gx, y: gy, width: gw, height: gh });
			absRect.set(group.id, { x: gx, y: gy, w: gw, h: gh });
		});

		// Build complete node list (groups first, then regular nodes)
		const allNodes: Node[] = [];

		// Add group nodes with positions (relative to parent if nested)
		groupNodes.forEach((group) => {
			const gp = groupPositions.get(group.id);
			if (!gp) return;

			let x = gp.x;
			let y = gp.y;

			// If nested, compute relative position to parent
			if (group.parent_id) {
				const parentPos = groupPositions.get(group.parent_id);
				if (parentPos) {
					x -= parentPos.x;
					y -= parentPos.y;
				}
			}

			allNodes.push({
				id: group.id,
				type: 'group',
				position: { x, y },
				width: gp.width,
				height: gp.height,
				data: { label: group.name, depth: groupDepthMap.get(group.id) ?? 0, metadata: group.metadata, spotlightDimmed: spotlightActive },
				parentId: group.parent_id,
				extent: group.parent_id ? 'parent' : undefined,
				style: `width: ${gp.width}px; height: ${gp.height}px;`,
				zIndex: -1
			});
		});

		// Add regular nodes with positions (relative to parent group if any)
		nodes.forEach((node) => {
			const rect = absRect.get(node.id);
			let x = rect ? rect.x : 0;
			let y = rect ? rect.y : 0;

			// If node belongs to a group, compute relative position
			if (node.parentId) {
				const parentPos = groupPositions.get(node.parentId);
				if (parentPos) {
					x -= parentPos.x;
					y -= parentPos.y;
				}
			}

			allNodes.push({
				...node,
				targetPosition: Position.Left,
				sourcePosition: Position.Right,
				position: { x, y },
				extent: node.parentId ? 'parent' : undefined
			});
		});

		return { nodes: allNodes, edges };
	}

	// Convert topology to Svelte Flow nodes and edges
	const { nodes, edges } = $derived.by((): { nodes: Node[]; edges: Edge[] } => {
		if (!topology) return { nodes: [], edges: [] };

		// Create raw nodes without positions
		const rawNodes: Node[] = [];

		// Build set of known group IDs so we only assign parentId when the group exists
		const knownGroupIds = new Set(groups.map(g => g.id));

		// Create place nodes
		for (const place of topology.places) {
			const tokens = marking.get(place.id) ?? [];
			// Get group_id from place — only use it if the group is actually present
			const rawGroupId = (place as any).group_id as string | undefined;
			const groupId = rawGroupId && knownGroupIds.has(rawGroupId) ? rawGroupId : undefined;

			// Spotlight role for this place
			const spotlightRole = spotlight
				? spotlight.consumedPlaceIds.includes(place.id) ? 'consumed' as const
					: spotlight.producedPlaceIds.includes(place.id) ? 'produced' as const
					: spotlight.targetPlaceId === place.id ? 'target' as const
					: 'dimmed' as const
				: null;

			// Pulse role for timeline stepping
			const pulseRole = markingDiff
				? markingDiff.appeared.includes(place.id) ? 'appeared' as const
					: markingDiff.disappeared.includes(place.id) ? 'disappeared' as const
					: null
				: null;

			rawNodes.push({
				id: placeNodeId(place.id),
				type: 'place',
				position: { x: 0, y: 0 }, // Will be set by dagre
				parentId: groupId,
				data: {
					label: place.name,
					kind: (place as any).kind ?? 'internal',
					tokens,
					bridgedOutTokens: bridgedOutTokens?.get(place.id) ?? [],
					bridgeTarget: (place as any).kind === 'bridge_out'
						? { target_net_id: (place as any).target_net_id, target_place_name: (place as any).target_place_name, reply_to: (place as any).reply_to }
						: null,
					bridgeSource: (place as any).kind === 'bridge_in' && (place as any).source_net_id
						? { source_net_id: (place as any).source_net_id, source_place_name: (place as any).source_place_name }
						: null,
					issueLevel: issuesMap.get(place.id) ?? null,
					selected: selectedElementId === place.id,
					spotlightRole,
					pulseRole,
					onSelect: () => onSelectPlace?.(place.id),
					onSelectToken: (tokenId: string) => onSelectToken?.(place.id, tokenId)
				}
			});
		}

		// Create transition nodes with port data
		for (const transition of topology.transitions) {
			const isEnabled = enabledTransitions.includes(transition.id);
			// Access ports from the transition (may be undefined for old format)
			const inputPorts = (transition as any).input_ports ?? [];
			const outputPorts = (transition as any).output_ports ?? [];
			const script = (transition as any).script ?? '';
			// Get status for this transition
			const status = transitionStatuses[transition.id];
			// Get group_id from transition — only use it if the group is actually present
			const rawGroupId = (transition as any).group_id as string | undefined;
			const groupId = rawGroupId && knownGroupIds.has(rawGroupId) ? rawGroupId : undefined;

			const effectHandlerId = (transition as any).effect_handler_id as string | undefined;
			const logicType = effectHandlerId ? 'effect' : 'rhai';
			const handlerId = effectHandlerId ?? null;

			// Build causation signal info for port rendering (only when visible)
			const causedSignalIds = (transition as any).caused_signals as string[] | undefined;
			const causedSignals = showCausation
				? (causedSignalIds
					?.map((sigId: string) => {
						const place = topology!.places.find(p => p.id === sigId);
						return place ? { id: sigId, name: place.name } : null;
					})
					.filter((s): s is { id: string; name: string } => s !== null) ?? [])
				: [];

			// Spotlight role for this transition
			const spotlightRole = spotlight
				? spotlight.transitionId === transition.id ? 'fired' as const : 'dimmed' as const
				: null;

			// Pulse role for timeline stepping
			const pulseRole = markingDiff?.firedTransition === transition.id ? 'fired' as const : null;

			// Compute actual height based on port count
			const transHeight = getTransitionHeight(inputPorts.length, outputPorts.length, causedSignals.length);

			rawNodes.push({
				id: transNodeId(transition.id),
				type: 'transition',
				position: { x: 0, y: 0 }, // Will be set by dagre
				parentId: groupId,
				data: {
					label: transition.name,
					enabled: isEnabled,
					inputPorts,
					outputPorts,
					causedSignals,
					guard: transition.guard,
					script,
					logicType,
					handlerId,
					status,
					issueLevel: issuesMap.get(transition.id) ?? null,
					selected: selectedElementId === transition.id,
					spotlightRole,
					pulseRole,
					onFire: () => onFireTransition(transition.id),
					onSelect: () => onSelectTransition?.(transition.id),
					_dims: { width: TRANSITION_WIDTH, height: transHeight }
				}
			});
		}

		// Create edges from arcs with port handles
		const rawEdges: Edge[] = [];
		
		// Standard token flow edges
		topology.arcs.forEach((arc) => {
			const isInput = arc.direction === 'place_to_transition';
			const portName = (arc as any).port_name;
			const isRead = !!(arc as any).read;

			// Skip read arcs when hidden
			if (isRead && !showReadArcs) return;

			rawEdges.push({
				id: arc.id,
				source: isInput ? placeNodeId(arc.place_id) : transNodeId(arc.transition_id),
				target: isInput ? transNodeId(arc.transition_id) : placeNodeId(arc.place_id),
				// Use port name as handle ID for transitions
				sourceHandle: isInput ? undefined : portName,
				targetHandle: isInput ? portName : undefined,
				type: 'default',
				animated: !isRead,
				style: isRead
					? 'stroke-width: 1.5px; stroke-dasharray: 4,3; stroke: #8b5cf6; opacity: 0.75;'
					: 'stroke-width: 2px;',
				...(isRead ? { markerEnd: { type: MarkerType.ArrowClosed, color: '#8b5cf6' } } : {}),
			});
		});

		// Causation edges (dashed, orange) from transition metadata
		if (showCausation) {
			for (const transition of topology.transitions) {
				const causedSignals = (transition as any).caused_signals as string[] | undefined;
				if (causedSignals) {
					for (const signalPlaceId of causedSignals) {
						const targetPlace = topology.places.find(p => p.id === signalPlaceId);
						if (targetPlace) {
							rawEdges.push({
								id: `cause-${transition.id}-${signalPlaceId}`,
								source: transNodeId(transition.id),
								sourceHandle: `cause-${signalPlaceId}`,
								target: placeNodeId(signalPlaceId),
								type: 'default',
								animated: false,
								style: 'stroke-width: 1.5px; stroke-dasharray: 5,5; stroke: #f97316; opacity: 0.8;',
								markerEnd: { type: MarkerType.ArrowClosed, color: '#f97316' },
								data: { isCausation: true }
							});
						}
					}
				}
			}
		}

		// Remote net nodes and bridge edges (grouped by remote netId)
		if (showBridges) {
			const remoteNets = new Map<string, {
				targets: Array<{ placeName: string; bridgePlaceId: string; groupId?: string }>;
				sources: Array<{ placeName: string; bridgePlaceId: string; groupId?: string }>;
			}>();

			for (const place of topology.places) {
				const kind = (place as any).kind as string;
				const rawGroupId = (place as any).group_id as string | undefined;
				const groupId = rawGroupId && knownGroupIds.has(rawGroupId) ? rawGroupId : undefined;

				if (kind === 'bridge_out') {
					const label = (place as any).label as string | undefined;
					const targetNetId = (place as any).target_net_id as string | undefined;
					const displayNetId = label || targetNetId;
					const targetPlaceName = (place as any).target_place_name as string | undefined;
					if (displayNetId && targetPlaceName) {
						if (!remoteNets.has(displayNetId)) {
							remoteNets.set(displayNetId, { targets: [], sources: [] });
						}
						remoteNets.get(displayNetId)!.targets.push({
							placeName: targetPlaceName,
							bridgePlaceId: place.id,
							groupId
						});
					}
				}

				if (kind === 'bridge_in') {
					const sourceNetId = (place as any).source_net_id as string | undefined;
					const sourcePlaceName = (place as any).source_place_name as string | undefined;
					if (sourceNetId && sourcePlaceName) {
						if (!remoteNets.has(sourceNetId)) {
							remoteNets.set(sourceNetId, { targets: [], sources: [] });
						}
						remoteNets.get(sourceNetId)!.sources.push({
							placeName: sourcePlaceName,
							bridgePlaceId: place.id,
							groupId
						});
					}
				}
			}

			const ROW_HEIGHT = 18;
			const PADDING_TOP = 30;
			const PADDING_BOTTOM = 10;
			const REMOTE_NET_WIDTH = 180;

			for (const [remoteLabel, entry] of remoteNets) {
				const remoteNodeId = `remote-net-${remoteLabel}`;

				// Deduplicate handles by placeName
				const seenTargets = new Set<string>();
				const uniqueTargets = entry.targets.filter(t => {
					if (seenTargets.has(t.placeName)) return false;
					seenTargets.add(t.placeName);
					return true;
				});
				const seenSources = new Set<string>();
				const uniqueSources = entry.sources.filter(s => {
					if (seenSources.has(s.placeName)) return false;
					seenSources.add(s.placeName);
					return true;
				});

				const targets = uniqueTargets.map(t => ({
					placeName: t.placeName,
					handleId: `target-${t.placeName}`
				}));
				const sources = uniqueSources.map(s => ({
					placeName: s.placeName,
					handleId: `source-${s.placeName}`
				}));

				const maxHandles = Math.max(targets.length, sources.length, 1);
				const computedHeight = PADDING_TOP + maxHandles * ROW_HEIGHT + PADDING_BOTTOM;

				// Inherit group from bridge_out places (prefer first match)
				const bridgeGroupId = entry.targets.find(t => t.groupId)?.groupId
					?? entry.sources.find(s => s.groupId)?.groupId;

				// Extract child net IDs from the spawned confirmation tokens in the marking.
				// For a bridge_out place like "worker_outbox", the sibling "worker_spawned"
				// place holds tokens with { child_net_id: "uuid" }.
				const childNetIds: string[] = [];
				for (const t of entry.targets) {
					const spawnedPlaceId = t.bridgePlaceId.replace(/_outbox$/, '_spawned');
					const spawnedTokens = marking.get(spawnedPlaceId) ?? [];
					for (const tok of spawnedTokens) {
						const val = (tok.color as any)?.value;
						if (val?.child_net_id && typeof val.child_net_id === 'string') {
							childNetIds.push(val.child_net_id);
						}
					}
				}

				rawNodes.push({
					id: remoteNodeId,
					type: 'remotenet',
					position: { x: 0, y: 0 },
					parentId: bridgeGroupId,
					data: {
						netId: remoteLabel,
						targets,
						sources,
						spotlightDimmed: spotlight != null && spotlight.allNodeIds.length > 0,
						selected: selectedElementId === remoteNodeId,
						childNetIds: childNetIds.length > 0 ? childNetIds : undefined,
						onNavigateToChild,
						onSelect: () => onSelectRemoteNet?.(
							remoteNodeId,
							remoteLabel,
							targets.map(t => t.placeName),
							sources.map(s => s.placeName),
							childNetIds
						),
						_dims: { width: REMOTE_NET_WIDTH, height: computedHeight }
					}
				});

				// Edges: bridge_out place → RemoteNet target handle
				for (const t of entry.targets) {
					rawEdges.push({
						id: `bridge-edge-out-${t.bridgePlaceId}`,
						source: placeNodeId(t.bridgePlaceId),
						target: remoteNodeId,
						targetHandle: `target-${t.placeName}`,
						type: 'default',
						animated: false,
						style: 'stroke-width: 1.5px; stroke-dasharray: 6,4; stroke: #14b8a6; opacity: 0.7;',
						markerEnd: { type: MarkerType.ArrowClosed, color: '#14b8a6' }
					});
				}

				// Edges: RemoteNet source handle → bridge_in place
				for (const s of entry.sources) {
					rawEdges.push({
						id: `bridge-edge-in-${s.bridgePlaceId}`,
						source: remoteNodeId,
						sourceHandle: `source-${s.placeName}`,
						target: placeNodeId(s.bridgePlaceId),
						type: 'default',
						animated: false,
						style: 'stroke-width: 1.5px; stroke-dasharray: 6,4; stroke: #14b8a6; opacity: 0.7;',
						markerEnd: { type: MarkerType.ArrowClosed, color: '#14b8a6' }
					});
				}
			}
		}

		// Dim non-participating edges when spotlight is active
		if (spotlight && spotlight.allNodeIds.length > 0) {
			const activePrefixed = new Set<string>();
			if (spotlight.transitionId) activePrefixed.add(transNodeId(spotlight.transitionId));
			for (const pid of [...spotlight.consumedPlaceIds, ...spotlight.producedPlaceIds]) {
				activePrefixed.add(placeNodeId(pid));
			}
			if (spotlight.targetPlaceId) activePrefixed.add(placeNodeId(spotlight.targetPlaceId));

			for (const edge of rawEdges) {
				if (!activePrefixed.has(edge.source) && !activePrefixed.has(edge.target)) {
					edge.style = (edge.style ?? '') + ' opacity: 0.12;';
				}
			}
		}

		// Apply group collapse if enabled
		let layoutNodes = rawNodes;
		let layoutEdges = rawEdges;
		let layoutGroups = groups;
		if (collapseGroups && groups.length > 0) {
			const collapsed = applyGroupCollapse(rawNodes, rawEdges, groups, selectedElementId, onSelectGroup, {
				childrenByParent: spawnChildren ?? new Map(),
				parentNetId: netId ?? '',
				onNavigateToChild,
			});
			layoutNodes = collapsed.nodes;
			layoutEdges = collapsed.edges;
			layoutGroups = collapsed.groups;
		}

		// Apply dagre layout with groups
		const spotlightActive = spotlight != null && spotlight.allNodeIds.length > 0;
		return getLayoutedElements(layoutNodes, layoutEdges, layoutGroups, spotlightActive);
	});
</script>

<div id="lab-canvas" class="lab-canvas w-full h-full relative">
	<SvelteFlow {nodes} {edges} {nodeTypes} fitView {colorMode} minZoom={0.05}>
		<CanvasController {spotlight} />
		<Background />
		{#if !presentationMode}
		<NodeSearch {topology} {onSelectPlace} {onSelectTransition} />
		<Controls />
		<MiniMap
			nodeColor={(node) => {
				if (node.type === 'place') return 'hsl(211 49% 65%)';
				if (node.type === 'transition') return 'hsl(215 15% 45%)';
				if (node.type === 'metagroup') return 'hsl(211 49% 55%)';
				if (node.type === 'remotenet') return 'hsl(168 50% 55%)';
				return 'transparent';
			}}
			maskStrokeColor="hsl(211 49% 60%)"
			maskStrokeWidth={2}
		/>
		{/if}
	</SvelteFlow>

	<!-- Canvas overlay toggles -->
	{#if !presentationMode}
		<div class="absolute bottom-2 left-14 z-10 flex items-center gap-1.5">
			<Tooltip.Root>
				<Tooltip.Trigger>
					<button
						class="canvas-toggle flex items-center gap-1.5 px-2 py-1.5 rounded-md text-xs font-medium border transition-colors
							{showCausation
								? 'bg-orange-50 border-orange-300 text-orange-700 hover:bg-orange-100 dark:bg-orange-950 dark:border-orange-700 dark:text-orange-300 dark:hover:bg-orange-900'
								: 'bg-card border-border text-muted-foreground hover:bg-accent'}"
						onclick={() => (showCausation = !showCausation)}
					>
						<svg class="w-3.5 h-3.5" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
							<path d="M2 8h8" stroke-dasharray="3,2" />
							<path d="M11 5l3 3-3 3" />
						</svg>
						Causes
					</button>
				</Tooltip.Trigger>
				<Tooltip.Content side="top">
					<span class="text-xs">{showCausation ? 'Hide' : 'Show'} causation arcs</span>
				</Tooltip.Content>
			</Tooltip.Root>

			<Tooltip.Root>
				<Tooltip.Trigger>
					<button
						class="canvas-toggle flex items-center gap-1.5 px-2 py-1.5 rounded-md text-xs font-medium border transition-colors
							{showBridges
								? 'bg-teal-50 border-teal-300 text-teal-700 hover:bg-teal-100 dark:bg-teal-950 dark:border-teal-700 dark:text-teal-300 dark:hover:bg-teal-900'
								: 'bg-card border-border text-muted-foreground hover:bg-accent'}"
						onclick={() => (showBridges = !showBridges)}
					>
						<svg class="w-3.5 h-3.5" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
							<path d="M2 8h4" stroke-dasharray="2,2" />
							<rect x="6" y="5" width="4" height="6" rx="1" />
							<path d="M10 8h4" stroke-dasharray="2,2" />
						</svg>
						Bridges
					</button>
				</Tooltip.Trigger>
				<Tooltip.Content side="top">
					<span class="text-xs">{showBridges ? 'Hide' : 'Show'} bridge connections</span>
				</Tooltip.Content>
			</Tooltip.Root>

			<Tooltip.Root>
				<Tooltip.Trigger>
					<button
						class="canvas-toggle flex items-center gap-1.5 px-2 py-1.5 rounded-md text-xs font-medium border transition-colors
							{showReadArcs
								? 'bg-violet-50 border-violet-300 text-violet-700 hover:bg-violet-100 dark:bg-violet-950 dark:border-violet-700 dark:text-violet-300 dark:hover:bg-violet-900'
								: 'bg-card border-border text-muted-foreground hover:bg-accent'}"
						onclick={() => (showReadArcs = !showReadArcs)}
					>
						<svg class="w-3.5 h-3.5" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
							<path d="M2 8h8" stroke-dasharray="4,3" />
							<path d="M11 5l3 3-3 3" />
						</svg>
						Read
					</button>
				</Tooltip.Trigger>
				<Tooltip.Content side="top">
					<span class="text-xs">{showReadArcs ? 'Hide' : 'Show'} read arcs</span>
				</Tooltip.Content>
			</Tooltip.Root>

			{#if groups.length > 0}
				<Tooltip.Root>
					<Tooltip.Trigger>
						<button
							class="canvas-toggle flex items-center gap-1.5 px-2 py-1.5 rounded-md text-xs font-medium border transition-colors
								{collapseGroups
									? 'bg-sky-50 border-sky-300 text-sky-700 hover:bg-sky-100 dark:bg-sky-950 dark:border-sky-700 dark:text-sky-300 dark:hover:bg-sky-900'
									: 'bg-card border-border text-muted-foreground hover:bg-accent'}"
							onclick={() => (collapseGroups = !collapseGroups)}
						>
							<svg class="w-3.5 h-3.5" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
								<rect x="1" y="1" width="6" height="6" rx="1" />
								<rect x="9" y="1" width="6" height="6" rx="1" />
								<rect x="5" y="9" width="6" height="6" rx="1" />
							</svg>
							Collapse
						</button>
					</Tooltip.Trigger>
					<Tooltip.Content side="top">
						<span class="text-xs">{collapseGroups ? 'Expand' : 'Collapse'} groups into summary nodes</span>
					</Tooltip.Content>
				</Tooltip.Root>
			{/if}
		</div>
	{/if}
</div>

<style>
	.canvas-toggle {
		box-shadow: 0 1px 4px rgba(0, 0, 0, 0.1);
	}

	.lab-canvas :global(.svelte-flow) {
		background-color: var(--background);

		/* Override SvelteFlow variables to match our theme */
		--xy-background-color: var(--background);
		--xy-background-pattern-dots-color: var(--flow-dots);

		/* Controls */
		--xy-controls-button-background-color: var(--card);
		--xy-controls-button-background-color-hover: var(--accent);
		--xy-controls-button-color: var(--foreground);
		--xy-controls-button-color-hover: var(--foreground);
		--xy-controls-button-border-color: var(--border);
		--xy-controls-box-shadow: 0 0 2px 1px rgba(0, 0, 0, 0.08);

		/* Minimap */
		--xy-minimap-background-color: var(--card);
		--xy-minimap-mask-background-color: var(--flow-minimap-mask);
		--xy-minimap-node-background-color: var(--muted-foreground);
		--xy-minimap-node-stroke-color: transparent;

		/* Edges */
		--xy-edge-stroke: var(--flow-edge);
		--xy-edge-stroke-selected: var(--primary);

		/* Edge labels */
		--xy-edge-label-background-color: var(--card);
		--xy-edge-label-color: var(--foreground);

		/* Attribution */
		--xy-attribution-background-color: color-mix(in srgb, var(--foreground) 15%, transparent);
	}
</style>
