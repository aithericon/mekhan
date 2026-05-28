import type { InstanceChild } from '$lib/api/client';

/**
 * Group sub-workflow child instances by the `parent_node_id` of the
 * SubWorkflow node that spawned them, ordered within each node by `spawn_seq`
 * (i.e. spawn / Loop-Map iteration order). Children without a `parent_node_id`
 * (shouldn't happen for spawned children, but the field is nullable on the
 * wire) are skipped. The instance graph view uses this to offer an "Enter
 * sub-workflow" drill-in per SubWorkflow node — one entry per run.
 */
export function groupChildrenByNode(children: InstanceChild[]): Map<string, InstanceChild[]> {
	const map = new Map<string, InstanceChild[]>();
	for (const c of children) {
		if (!c.parent_node_id) continue;
		const list = map.get(c.parent_node_id) ?? [];
		list.push(c);
		map.set(c.parent_node_id, list);
	}
	for (const list of map.values()) {
		list.sort((a, b) => (a.spawn_seq ?? 0) - (b.spawn_seq ?? 0));
	}
	return map;
}
