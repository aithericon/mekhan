/**
 * Pure group-collapse transform for the Petri canvas.
 *
 * Folds every root scenario group (and its descendants) into a single
 * `metagroup` node with boundary input/output ports, re-routing crossing
 * edges to the meta-node handles. Extracted verbatim from LabCanvas — no DOM,
 * no reactivity.
 */

import type { Node, Edge } from '@xyflow/svelte';
import type { ScenarioGroup } from '$lib/types/petri';
import { META_WIDTH, getMetaHeight } from './graph-layout';

/** Display label for a node by its prefixed id (falls back to raw id). */
function getNodeLabel(prefixedId: string, nodes: Node[]): string {
	const node = nodes.find((n) => n.id === prefixedId);
	return (node?.data as any)?.label ?? prefixedId.replace(/^[pt]:/, '');
}

export interface SpawnInfo {
	childrenByParent: Map<string, { netId: string; label: string }[]>;
	parentNetId: string;
	onNavigateToChild?: (netId: string) => void;
}

export function applyGroupCollapse(
	rawNodes: Node[],
	rawEdges: Edge[],
	groupDefs: ScenarioGroup[],
	selectedId: string | null | undefined,
	onSelect: ((groupId: string) => void) | undefined,
	spawnInfo?: SpawnInfo
): { nodes: Node[]; edges: Edge[]; groups: ScenarioGroup[] } {
	const rootGroups = groupDefs.filter((g) => !g.parent_id);
	if (rootGroups.length === 0) {
		return { nodes: rawNodes, edges: rawEdges, groups: groupDefs };
	}

	// Build descendant group sets for each root group.
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

	// Build member sets (node ids inside each root group).
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

	const internalEdgeIds = new Set<string>();
	const metaNodes: Node[] = [];

	function findRootGroup(nodeId: string): string | undefined {
		for (const [rootId, members] of memberSets) {
			if (members.has(nodeId)) return rootId;
		}
		return undefined;
	}

	// First pass: classify edges and collect boundary info per root group.
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
			// Fully internal to one group.
			internalEdgeIds.add(edge.id);
		} else if (srcRoot && tgtRoot && srcRoot !== tgtRoot) {
			// Edge between two collapsed groups — re-route both ends.
			const metaTgtId = `meta-${tgtRoot}`;
			outputPortsMap
				.get(srcRoot)!
				.set(metaTgtId, rootGroups.find((g) => g.id === tgtRoot)!.name);
			edge.source = `meta-${srcRoot}`;
			edge.sourceHandle = `out-${metaTgtId}`;
			const metaSrcId = `meta-${srcRoot}`;
			inputPortsMap
				.get(tgtRoot)!
				.set(metaSrcId, rootGroups.find((g) => g.id === srcRoot)!.name);
			edge.target = `meta-${tgtRoot}`;
			edge.targetHandle = `in-${metaSrcId}`;
		} else if (!srcRoot && tgtRoot) {
			// External → inside group (input boundary).
			inputPortsMap.get(tgtRoot)!.set(edge.source, getNodeLabel(edge.source, rawNodes));
			edge.target = `meta-${tgtRoot}`;
			edge.targetHandle = `in-${edge.source}`;
		} else if (srcRoot && !tgtRoot) {
			// Inside group → external (output boundary).
			outputPortsMap.get(srcRoot)!.set(edge.target, getNodeLabel(edge.target, rawNodes));
			edge.source = `meta-${srcRoot}`;
			edge.sourceHandle = `out-${edge.target}`;
		}
	}

	// Create meta-nodes.
	for (const root of rootGroups) {
		const members = memberSets.get(root.id)!;

		let placeCount = 0,
			transitionCount = 0,
			tokenCount = 0;
		for (const nodeId of members) {
			const node = rawNodes.find((n) => n.id === nodeId);
			if (!node) continue;
			if (node.type === 'place') {
				placeCount++;
				tokenCount +=
					((node.data as any).tokens?.length ?? 0) +
					((node.data as any).bridgedOutTokens?.length ?? 0);
			} else if (node.type === 'transition') {
				transitionCount++;
			}
		}

		const inputPorts = [...inputPortsMap.get(root.id)!].map(([id, label]) => ({
			id: `in-${id}`,
			label
		}));
		const outputPorts = [...outputPortsMap.get(root.id)!].map(([id, label]) => ({
			id: `out-${id}`,
			label
		}));
		const metaHeight = getMetaHeight(inputPorts.length, outputPorts.length);

		const isSpawn = root.metadata?.spawn === true;
		const childNets =
			isSpawn && spawnInfo
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
				spawnChildNetIds: isSpawn ? childNets.map((c) => c.netId) : undefined,
				onNavigateToChild: isSpawn ? spawnInfo?.onNavigateToChild : undefined,
				_dims: { width: META_WIDTH, height: metaHeight }
			}
		});
	}

	const filteredNodes = rawNodes.filter((n) => !allMembers.has(n.id));
	const filteredEdges = rawEdges.filter((e) => !internalEdgeIds.has(e.id));

	return {
		nodes: [...filteredNodes, ...metaNodes],
		edges: filteredEdges,
		groups: [] // No group boxes when collapsed.
	};
}
