import { createLabStore, type LabStore } from './lab.svelte';

export interface NetTab {
	netId: string;
	label: string;
	store: LabStore;
}

export interface NetMeta {
	netId: string;
	label: string;
	status: string;
	inMemory: boolean;
	templateId?: string;
	createdBy?: string;
}

export interface NetTreeNode {
	meta: NetMeta;
	children: NetTreeNode[];
}

export type StatusFilter = 'active' | 'all';

let nets = $state<NetTab[]>([]);
let allNetsMeta = $state<NetMeta[]>([]);
let statusFilter = $state<StatusFilter>('active');
let activeNetId = $state<string>('');

const netsMeta = $derived(
	statusFilter === 'all'
		? allNetsMeta
		: allNetsMeta.filter((m) => m.status === 'running' || m.status === 'created')
);

const activeStore = $derived.by(() => {
	return nets.find((n) => n.netId === activeNetId)?.store ?? null;
});

/** Build a parent-child tree from flat metadata using created_by. */
function buildTree(metas: NetMeta[]): NetTreeNode[] {
	const metaMap = new Map<string, NetMeta>();
	for (const m of metas) metaMap.set(m.netId, m);

	const childrenMap = new Map<string, NetTreeNode[]>();
	const roots: NetTreeNode[] = [];

	for (const m of metas) {
		const node: NetTreeNode = { meta: m, children: [] };

		// Parse "spawn:{parent_net_id}" format
		const parentId = m.createdBy?.startsWith('spawn:')
			? m.createdBy.slice(6)
			: undefined;

		if (parentId && metaMap.has(parentId)) {
			if (!childrenMap.has(parentId)) childrenMap.set(parentId, []);
			childrenMap.get(parentId)!.push(node);
		} else {
			roots.push(node);
		}
	}

	// Attach children to nodes
	function attachChildren(nodes: NetTreeNode[]) {
		for (const node of nodes) {
			node.children = childrenMap.get(node.meta.netId) ?? [];
			attachChildren(node.children);
		}
	}
	attachChildren(roots);

	return roots;
}

const tree = $derived(buildTree(netsMeta));

/** Group child nets by parent net ID for spawn instance counts. */
const spawnChildren = $derived.by(() => {
	const map = new Map<string, NetMeta[]>();
	for (const meta of netsMeta) {
		if (meta.createdBy?.startsWith('spawn:')) {
			const parentId = meta.createdBy.slice(6);
			if (!map.has(parentId)) map.set(parentId, []);
			map.get(parentId)!.push(meta);
		}
	}
	return map;
});

/** Fetch all nets with metadata from the backend and sync tabs. */
async function fetchNets() {
	try {
		const response = await fetch('/api/nets/metadata');
		if (!response.ok) {
			console.error('Failed to fetch nets metadata:', response.status);
			return;
		}

		const metadata: Array<{
			net_id: string;
			status: string;
			in_memory: boolean;
			template_id?: string;
			parameters?: unknown;
			created_by?: string;
			label?: string;
		}> = await response.json();

		// Store all metadata — the derived `netsMeta` applies the status filter
		allNetsMeta = metadata.map((m) => ({
			netId: m.net_id,
			label: m.label ?? m.net_id,
			status: m.status,
			inMemory: m.in_memory,
			templateId: m.template_id,
			createdBy: m.created_by
		}));

		// Sync tabs: add new nets that are visible under current filter, update labels
		const visibleIds = new Set(netsMeta.map((m) => m.netId));
		for (const m of netsMeta) {
			const existing = nets.find((n) => n.netId === m.netId);
			if (!existing) {
				nets = [
					...nets,
					{
						netId: m.netId,
						label: m.label,
						store: createLabStore(m.netId)
					}
				];
			} else if (m.label && existing.label !== m.label) {
				existing.label = m.label;
			}
		}

		// Remove tabs for nets no longer visible under current filter
		const stale = nets.filter((n) => !visibleIds.has(n.netId));
		if (stale.length > 0) {
			for (const s of stale) s.store.stopLiveUpdates();
			nets = nets.filter((n) => visibleIds.has(n.netId));
		}

		// If we have nets but no active tab, select the first one
		if ((!activeNetId || !nets.find((n) => n.netId === activeNetId)) && nets.length > 0) {
			activeNetId = nets[0].netId;
		}
	} catch (e) {
		console.error('Failed to fetch nets:', e);
	}
}

/** Switch to a net tab by ID. */
function setActive(netId: string) {
	if (nets.find((n) => n.netId === netId)) {
		activeNetId = netId;
	}
}

/** Add a new net tab (creates store). Returns the store. */
function addNet(netId: string, label?: string): LabStore {
	const existing = nets.find((n) => n.netId === netId);
	if (existing) return existing.store;

	const store = createLabStore(netId);
	nets = [...nets, { netId, label: label ?? netId, store }];
	return store;
}

/** Remove a net tab. Calls the backend to terminate/clean up, then removes locally. */
async function removeNet(netId: string): Promise<boolean> {
	if (nets.length <= 1) return false;

	try {
		const response = await fetch(`/api/nets/${netId}`, { method: 'DELETE' });
		if (!response.ok && response.status !== 404) {
			console.error(`Failed to delete net ${netId}:`, response.status);
			return false;
		}
	} catch (e) {
		console.error(`Failed to delete net ${netId}:`, e);
		return false;
	}

	const tab = nets.find((n) => n.netId === netId);
	if (tab) {
		tab.store.stopLiveUpdates();
	}
	nets = nets.filter((n) => n.netId !== netId);
	if (activeNetId === netId) {
		activeNetId = nets[0]?.netId ?? '';
	}
	return true;
}

/** Remove a net tab locally (no backend call). Use after hibernate or when syncing stale tabs. */
function removeTab(netId: string) {
	const tab = nets.find((n) => n.netId === netId);
	if (tab) {
		tab.store.stopLiveUpdates();
	}
	nets = nets.filter((n) => n.netId !== netId);
	if (activeNetId === netId) {
		activeNetId = nets[0]?.netId ?? '';
	}
}

/** Wake a hibernated net (replay events from NATS, reload into memory). */
async function wakeNet(netId: string): Promise<boolean> {
	try {
		const response = await fetch(`/api/nets/${netId}/command/wake`, {
			method: 'POST'
		});
		if (!response.ok) {
			console.error(`Failed to wake net ${netId}:`, response.status);
			return false;
		}
	} catch (e) {
		console.error(`Failed to wake net ${netId}:`, e);
		return false;
	}
	return true;
}

/** Check if a net is currently hibernated (in metadata but not in memory). */
function isHibernated(netId: string): boolean {
	const meta = allNetsMeta.find((m) => m.netId === netId);
	return meta ? !meta.inMemory : false;
}

/** Check if a net is in a terminal state (completed or cancelled). */
function isTerminal(netId: string): boolean {
	const meta = allNetsMeta.find((m) => m.netId === netId);
	return meta ? meta.status === 'completed' || meta.status === 'cancelled' : false;
}

/** Set the status filter for the net list. */
function setStatusFilter(filter: StatusFilter) {
	statusFilter = filter;
}

/** Hibernate a net (unload from memory, preserving it for later rehydration). */
async function hibernateNet(netId: string): Promise<boolean> {
	try {
		const response = await fetch(`/api/nets/${netId}/command/hibernate`, {
			method: 'POST'
		});
		if (!response.ok) {
			console.error(`Failed to hibernate net ${netId}:`, response.status);
			return false;
		}
	} catch (e) {
		console.error(`Failed to hibernate net ${netId}:`, e);
		return false;
	}
	return true;
}

/** Get the store for a specific net (or null if not found). */
function getStore(netId: string): LabStore | null {
	return nets.find((n) => n.netId === netId)?.store ?? null;
}

export const multiNetStore = {
	get nets() {
		return nets;
	},
	get netsMeta() {
		return netsMeta;
	},
	get tree() {
		return tree;
	},
	get activeNetId() {
		return activeNetId;
	},
	get activeStore() {
		return activeStore;
	},
	/** Map of parent net ID → child net metadata (for spawn instance counts). */
	get spawnChildren() {
		return spawnChildren;
	},
	get statusFilter() {
		return statusFilter;
	},
	fetchNets,
	setActive,
	addNet,
	removeNet,
	removeTab,
	hibernateNet,
	wakeNet,
	isHibernated,
	isTerminal,
	setStatusFilter,
	getStore
};
