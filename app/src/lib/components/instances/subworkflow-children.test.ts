import { describe, it, expect } from 'vitest';
import type { InstanceChild } from '$lib/api/client';
import { groupChildrenByNode } from './subworkflow-children';

function child(overrides: Partial<InstanceChild>): InstanceChild {
	return {
		id: overrides.id ?? crypto.randomUUID(),
		parent_node_id: overrides.parent_node_id ?? null,
		spawn_seq: overrides.spawn_seq ?? null,
		template_id: overrides.template_id ?? crypto.randomUUID(),
		template_version: overrides.template_version ?? 1,
		template_name: overrides.template_name ?? 'Child',
		status: overrides.status ?? 'completed',
		created_at: overrides.created_at ?? '2026-01-01T00:00:00Z',
		started_at: overrides.started_at ?? null,
		completed_at: overrides.completed_at ?? null
	};
}

describe('groupChildrenByNode', () => {
	it('groups children by parent_node_id', () => {
		const a1 = child({ parent_node_id: 'sub_a', spawn_seq: 1 });
		const b1 = child({ parent_node_id: 'sub_b', spawn_seq: 1 });
		const map = groupChildrenByNode([a1, b1]);
		expect([...map.keys()].sort()).toEqual(['sub_a', 'sub_b']);
		expect(map.get('sub_a')).toEqual([a1]);
		expect(map.get('sub_b')).toEqual([b1]);
	});

	it('orders a node\'s children by spawn_seq (loop/map iteration order)', () => {
		// Provided out of order; must come back ascending by spawn_seq.
		const third = child({ parent_node_id: 'sub', spawn_seq: 3 });
		const first = child({ parent_node_id: 'sub', spawn_seq: 1 });
		const second = child({ parent_node_id: 'sub', spawn_seq: 2 });
		const map = groupChildrenByNode([third, first, second]);
		expect(map.get('sub')?.map((c) => c.spawn_seq)).toEqual([1, 2, 3]);
	});

	it('skips children with no parent_node_id', () => {
		const orphan = child({ parent_node_id: null, spawn_seq: 1 });
		const real = child({ parent_node_id: 'sub', spawn_seq: 1 });
		const map = groupChildrenByNode([orphan, real]);
		expect(map.size).toBe(1);
		expect(map.get('sub')).toEqual([real]);
	});

	it('treats a null spawn_seq as 0 without throwing', () => {
		const withSeq = child({ parent_node_id: 'sub', spawn_seq: 2 });
		const noSeq = child({ parent_node_id: 'sub', spawn_seq: null });
		const map = groupChildrenByNode([withSeq, noSeq]);
		// null → 0, so the null-seq child sorts first.
		expect(map.get('sub')?.map((c) => c.spawn_seq)).toEqual([null, 2]);
	});

	it('returns an empty map for no children', () => {
		expect(groupChildrenByNode([]).size).toBe(0);
	});
});
