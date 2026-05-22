/**
 * Pure topology → Svelte Flow nodes/edges transform for the Petri canvas.
 *
 * This is the ~360-line `$derived.by` that used to live inline in
 * LabCanvas.svelte: it builds place/transition/remote-net nodes, token-flow /
 * causation / bridge edges, applies spotlight dimming and (optionally) group
 * collapse, then runs dagre layout. Extracted with no behaviour change; the
 * `(place as any)` / `(transition as any)` / `(arc as any)` casts are gone now
 * that the wire fields are modelled on the types.
 */

import { MarkerType, type Node, type Edge } from '@xyflow/svelte';
import type {
	EventSpotlight,
	IssueLevel,
	MarkingDiff,
	PetriNet,
	ScenarioGroup,
	Token,
	TransitionStatus,
	ValidationIssue
} from '$lib/types/petri';
import { getTransitionHeight, getLayoutedElements, TRANSITION_WIDTH } from './graph-layout';
import { applyGroupCollapse } from './group-collapse';

// SvelteFlow uses node.id as the key in keyed {#each} blocks. Places and
// transitions live in separate namespaces, so the same id can appear for
// both — prefix by type to guarantee uniqueness.
const placeNodeId = (id: string) => `p:${id}`;
const transNodeId = (id: string) => `t:${id}`;

const severityOrder: Record<IssueLevel, number> = { error: 0, warning: 1, info: 2 };

/** Map node id → highest-severity issue level. */
export function buildIssuesMap(issues: ValidationIssue[]): Map<string, IssueLevel> {
	const map = new Map<string, IssueLevel>();
	for (const issue of issues) {
		const nodeId = issue.node_id;
		if (!nodeId) continue;
		const existing = map.get(nodeId);
		if (!existing || severityOrder[issue.level] < severityOrder[existing]) {
			map.set(nodeId, issue.level);
		}
	}
	return map;
}

export interface TopologyToFlowOptions {
	topology: PetriNet | null;
	marking: Map<string, Token[]>;
	bridgedOutTokens?: Map<string, Token[]>;
	enabledTransitions: string[];
	transitionStatuses: Record<string, TransitionStatus>;
	issues: ValidationIssue[];
	groups: ScenarioGroup[];
	selectedElementId: string | null | undefined;
	spotlight: EventSpotlight | null | undefined;
	markingDiff: MarkingDiff | null | undefined;
	showCausation: boolean;
	showBridges: boolean;
	showReadArcs: boolean;
	collapseGroups: boolean;
	netId?: string;
	spawnChildren?: Map<string, { netId: string; label: string }[]>;
	onNavigateToChild?: (netId: string) => void;
	onFireTransition: (transitionId: string) => void;
	onSelectPlace?: (placeId: string) => void;
	onSelectTransition?: (transitionId: string) => void;
	onSelectToken?: (placeId: string, tokenId: string) => void;
	onSelectGroup?: (groupId: string) => void;
	onSelectRemoteNet?: (
		id: string,
		label: string,
		targets: string[],
		sources: string[],
		childNetIds: string[]
	) => void;
}

export function topologyToFlow(opts: TopologyToFlowOptions): {
	nodes: Node[];
	edges: Edge[];
} {
	const {
		topology,
		marking,
		bridgedOutTokens,
		enabledTransitions,
		transitionStatuses,
		issues,
		groups,
		selectedElementId,
		spotlight,
		markingDiff,
		showCausation,
		showBridges,
		showReadArcs,
		collapseGroups,
		netId,
		spawnChildren,
		onNavigateToChild,
		onFireTransition,
		onSelectPlace,
		onSelectTransition,
		onSelectToken,
		onSelectGroup,
		onSelectRemoteNet
	} = opts;

	if (!topology || !topology.places) return { nodes: [], edges: [] };

	const issuesMap = buildIssuesMap(issues);
	const rawNodes: Node[] = [];

	// Only assign parentId when the group exists.
	const knownGroupIds = new Set(groups.map((g) => g.id));

	// ── Place nodes ─────────────────────────────────────────────────────
	for (const place of topology.places) {
		const tokens = marking.get(place.id) ?? [];
		const rawGroupId = place.group_id ?? undefined;
		const groupId = rawGroupId && knownGroupIds.has(rawGroupId) ? rawGroupId : undefined;

		const spotlightRole = spotlight
			? spotlight.consumedPlaceIds.includes(place.id)
				? ('consumed' as const)
				: spotlight.producedPlaceIds.includes(place.id)
					? ('produced' as const)
					: spotlight.targetPlaceId === place.id
						? ('target' as const)
						: ('dimmed' as const)
			: null;

		const pulseRole = markingDiff
			? markingDiff.appeared.includes(place.id)
				? ('appeared' as const)
				: markingDiff.disappeared.includes(place.id)
					? ('disappeared' as const)
					: null
			: null;

		rawNodes.push({
			id: placeNodeId(place.id),
			type: 'place',
			position: { x: 0, y: 0 },
			parentId: groupId,
			data: {
				label: place.name,
				kind: place.kind ?? 'internal',
				tokens,
				bridgedOutTokens: bridgedOutTokens?.get(place.id) ?? [],
				bridgeTarget:
					place.kind === 'bridge_out'
						? {
								target_net_id: place.target_net_id,
								target_place_name: place.target_place_name,
								reply_to: place.reply_to
							}
						: null,
				bridgeSource:
					place.kind === 'bridge_in' && place.source_net_id
						? {
								source_net_id: place.source_net_id,
								source_place_name: place.source_place_name
							}
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

	// ── Transition nodes ────────────────────────────────────────────────
	for (const transition of topology.transitions) {
		const isEnabled = enabledTransitions.includes(transition.id);
		const inputPorts = transition.input_ports ?? [];
		const outputPorts = transition.output_ports ?? [];
		const script = transition.script ?? '';
		const status = transitionStatuses[transition.id];
		const rawGroupId = transition.group_id ?? undefined;
		const groupId = rawGroupId && knownGroupIds.has(rawGroupId) ? rawGroupId : undefined;

		const effectHandlerId = transition.effect_handler_id ?? undefined;
		const logicType = effectHandlerId ? 'effect' : 'rhai';
		const handlerId = effectHandlerId ?? null;

		const causedSignalIds = transition.caused_signals;
		const causedSignals = showCausation
			? (causedSignalIds
					?.map((sigId: string) => {
						const place = topology!.places.find((p) => p.id === sigId);
						return place ? { id: sigId, name: place.name } : null;
					})
					.filter((s): s is { id: string; name: string } => s !== null) ?? [])
			: [];

		const spotlightRole = spotlight
			? spotlight.transitionId === transition.id
				? ('fired' as const)
				: ('dimmed' as const)
			: null;

		const pulseRole = markingDiff?.firedTransition === transition.id ? ('fired' as const) : null;

		const transHeight = getTransitionHeight(
			inputPorts.length,
			outputPorts.length,
			causedSignals.length
		);

		rawNodes.push({
			id: transNodeId(transition.id),
			type: 'transition',
			position: { x: 0, y: 0 },
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

	// ── Token-flow edges ────────────────────────────────────────────────
	const rawEdges: Edge[] = [];

	topology.arcs.forEach((arc) => {
		const isInput = arc.direction === 'place_to_transition';
		const portName = arc.port_name;
		const isRead = !!arc.read;

		if (isRead && !showReadArcs) return;

		rawEdges.push({
			id: `${arc.place_id}-${arc.transition_id}-${arc.direction}`,
			source: isInput ? placeNodeId(arc.place_id) : transNodeId(arc.transition_id),
			target: isInput ? transNodeId(arc.transition_id) : placeNodeId(arc.place_id),
			sourceHandle: isInput ? undefined : portName,
			targetHandle: isInput ? portName : undefined,
			type: 'default',
			animated: !isRead,
			style: isRead
				? 'stroke-width: 1.5px; stroke-dasharray: 4,3; stroke: #8b5cf6; opacity: 0.75;'
				: 'stroke-width: 2px;',
			...(isRead ? { markerEnd: { type: MarkerType.ArrowClosed, color: '#8b5cf6' } } : {})
		});
	});

	// ── Causation edges ─────────────────────────────────────────────────
	if (showCausation) {
		for (const transition of topology.transitions) {
			const causedSignals = transition.caused_signals;
			if (causedSignals) {
				for (const signalPlaceId of causedSignals) {
					const targetPlace = topology.places.find((p) => p.id === signalPlaceId);
					if (targetPlace) {
						rawEdges.push({
							id: `cause-${transition.id}-${signalPlaceId}`,
							source: transNodeId(transition.id),
							sourceHandle: `cause-${signalPlaceId}`,
							target: placeNodeId(signalPlaceId),
							type: 'default',
							animated: false,
							style:
								'stroke-width: 1.5px; stroke-dasharray: 5,5; stroke: #f97316; opacity: 0.8;',
							markerEnd: { type: MarkerType.ArrowClosed, color: '#f97316' },
							data: { isCausation: true }
						});
					}
				}
			}
		}
	}

	// ── Remote-net nodes + bridge edges ─────────────────────────────────
	if (showBridges) {
		const remoteNets = new Map<
			string,
			{
				targets: Array<{ placeName: string; bridgePlaceId: string; groupId?: string }>;
				sources: Array<{ placeName: string; bridgePlaceId: string; groupId?: string }>;
			}
		>();

		for (const place of topology.places) {
			const kind = place.kind;
			const rawGroupId = place.group_id ?? undefined;
			const groupId = rawGroupId && knownGroupIds.has(rawGroupId) ? rawGroupId : undefined;

			if (kind === 'bridge_out') {
				const label = place.label ?? undefined;
				const targetNetId = place.target_net_id ?? undefined;
				const displayNetId = label || targetNetId;
				const targetPlaceName = place.target_place_name ?? undefined;
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
				const sourceNetId = place.source_net_id ?? undefined;
				const sourcePlaceName = place.source_place_name ?? undefined;
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

			const seenTargets = new Set<string>();
			const uniqueTargets = entry.targets.filter((t) => {
				if (seenTargets.has(t.placeName)) return false;
				seenTargets.add(t.placeName);
				return true;
			});
			const seenSources = new Set<string>();
			const uniqueSources = entry.sources.filter((s) => {
				if (seenSources.has(s.placeName)) return false;
				seenSources.add(s.placeName);
				return true;
			});

			const targets = uniqueTargets.map((t) => ({
				placeName: t.placeName,
				handleId: `target-${t.placeName}`
			}));
			const sources = uniqueSources.map((s) => ({
				placeName: s.placeName,
				handleId: `source-${s.placeName}`
			}));

			const maxHandles = Math.max(targets.length, sources.length, 1);
			const computedHeight = PADDING_TOP + maxHandles * ROW_HEIGHT + PADDING_BOTTOM;

			const bridgeGroupId =
				entry.targets.find((t) => t.groupId)?.groupId ??
				entry.sources.find((s) => s.groupId)?.groupId;

			// Extract child net ids from the spawned confirmation tokens. For a
			// bridge_out place like "worker_outbox", the sibling "worker_spawned"
			// place holds tokens with { child_net_id: "uuid" }.
			const childNetIds: string[] = [];
			for (const t of entry.targets) {
				const spawnedPlaceId = t.bridgePlaceId.replace(/_outbox$/, '_spawned');
				const spawnedTokens = marking.get(spawnedPlaceId) ?? [];
				for (const tok of spawnedTokens) {
					const val =
						tok.color.type === 'Data'
							? (tok.color.value as { child_net_id?: unknown } | null | undefined)
							: undefined;
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
					onSelect: () =>
						onSelectRemoteNet?.(
							remoteNodeId,
							remoteLabel,
							targets.map((t) => t.placeName),
							sources.map((s) => s.placeName),
							childNetIds
						),
					_dims: { width: REMOTE_NET_WIDTH, height: computedHeight }
				}
			});

			for (const t of entry.targets) {
				rawEdges.push({
					id: `bridge-edge-out-${t.bridgePlaceId}`,
					source: placeNodeId(t.bridgePlaceId),
					target: remoteNodeId,
					targetHandle: `target-${t.placeName}`,
					type: 'default',
					animated: false,
					style:
						'stroke-width: 1.5px; stroke-dasharray: 6,4; stroke: #14b8a6; opacity: 0.7;',
					markerEnd: { type: MarkerType.ArrowClosed, color: '#14b8a6' }
				});
			}

			for (const s of entry.sources) {
				rawEdges.push({
					id: `bridge-edge-in-${s.bridgePlaceId}`,
					source: remoteNodeId,
					sourceHandle: `source-${s.placeName}`,
					target: placeNodeId(s.bridgePlaceId),
					type: 'default',
					animated: false,
					style:
						'stroke-width: 1.5px; stroke-dasharray: 6,4; stroke: #14b8a6; opacity: 0.7;',
					markerEnd: { type: MarkerType.ArrowClosed, color: '#14b8a6' }
				});
			}
		}
	}

	// ── Spotlight dimming ───────────────────────────────────────────────
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

	// ── Group collapse + dagre layout ───────────────────────────────────
	let layoutNodes = rawNodes;
	let layoutEdges = rawEdges;
	let layoutGroups = groups;
	if (collapseGroups && groups.length > 0) {
		const collapsed = applyGroupCollapse(
			rawNodes,
			rawEdges,
			groups,
			selectedElementId,
			onSelectGroup,
			{
				childrenByParent: spawnChildren ?? new Map(),
				parentNetId: netId ?? '',
				onNavigateToChild
			}
		);
		layoutNodes = collapsed.nodes;
		layoutEdges = collapsed.edges;
		layoutGroups = collapsed.groups;
	}

	const spotlightActive = spotlight != null && spotlight.allNodeIds.length > 0;
	return getLayoutedElements(layoutNodes, layoutEdges, layoutGroups, spotlightActive);
}
