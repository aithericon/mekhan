import { describe, it, expect } from 'vitest';
import { groupFleet, filterFleetByGroup } from './grouping';
import type { RunnerSummary, RunnerPresenceSnapshot } from '$lib/api/runners';
import type { ResourceSummary } from '$lib/api/resources';

// Minimal fixture builders — only the fields groupFleet reads.
function runner(id: string, group: string | null, name = id): RunnerSummary {
	return {
		id,
		name,
		group: group ?? undefined,
		status: 'active',
		capabilities: {},
		enrolled_at: '2026-01-01T00:00:00Z'
	} as RunnerSummary;
}

function present(runner_id: string, backends: string[] = []): RunnerPresenceSnapshot {
	return { runner_id, present: true, last_seen_ms_ago: 100, backends } as RunnerPresenceSnapshot;
}

function groupResource(path: string): ResourceSummary {
	return {
		id: `res-${path}`,
		path,
		resource_type: 'capacity',
		display_name: path,
		latest_version: 1,
		created_at: '2026-01-01T00:00:00Z',
		updated_at: '2026-01-01T00:00:00Z'
	} as ResourceSummary;
}

describe('groupFleet', () => {
	it('orders sections backed → unbacked → ungrouped, backed sorted by alias', () => {
		const runners = [
			runner('a', 'zeta'),
			runner('b', 'alpha'),
			runner('c', 'ghost'), // unbacked
			runner('d', null) // ungrouped
		];
		const groups = [groupResource('zeta'), groupResource('alpha')];
		const sections = groupFleet(runners, {}, groups);

		expect(sections.map((s) => [s.kind, s.alias])).toEqual([
			['backed', 'alpha'],
			['backed', 'zeta'],
			['unbacked', 'ghost'],
			['ungrouped', null]
		]);
	});

	it('shows a backed group with zero members', () => {
		const sections = groupFleet([], {}, [groupResource('empty_group')]);
		expect(sections).toHaveLength(1);
		expect(sections[0]).toMatchObject({ kind: 'backed', alias: 'empty_group', onlineCount: 0 });
		expect(sections[0].runners).toEqual([]);
	});

	it('counts only PRESENT runners as online and unions their backends', () => {
		const runners = [runner('a', 'lab'), runner('b', 'lab'), runner('c', 'lab')];
		const presenceById = {
			a: present('a', ['python', 'docker']),
			b: present('b', ['python', 'loki']),
			c: { runner_id: 'c', present: false, last_seen_ms_ago: 99999, backends: ['gpu'] }
		} as Record<string, RunnerPresenceSnapshot>;
		const [lab] = groupFleet(runners, presenceById, [groupResource('lab')]);

		expect(lab.onlineCount).toBe(2); // c is offline
		// union of present a + b only; c's 'gpu' excluded; sorted + de-duped.
		expect(lab.backends).toEqual(['docker', 'loki', 'python']);
	});

	it('omits the ungrouped bucket when every runner has a group', () => {
		const sections = groupFleet([runner('a', 'lab')], {}, [groupResource('lab')]);
		expect(sections.some((s) => s.kind === 'ungrouped')).toBe(false);
	});

	it('puts a runner whose group has no backing resource in an unbacked section', () => {
		const sections = groupFleet([runner('a', 'orphan')], { a: present('a') }, []);
		expect(sections).toHaveLength(1);
		expect(sections[0]).toMatchObject({ kind: 'unbacked', alias: 'orphan', resource: null });
	});
});

describe('filterFleetByGroup', () => {
	const runners = [runner('a', 'lab'), runner('b', 'lab'), runner('c', 'gpu'), runner('d', null)];
	const groups = [groupResource('lab'), groupResource('gpu')];

	it('is a no-op when alias is null/undefined (preserves unfiltered callers)', () => {
		expect(filterFleetByGroup(runners, groups, null)).toEqual({
			runners,
			groupResources: groups
		});
		expect(filterFleetByGroup(runners, groups, undefined)).toEqual({
			runners,
			groupResources: groups
		});
	});

	it('keeps only the matching group’s runners + backing resource', () => {
		const { runners: rs, groupResources: gs } = filterFleetByGroup(runners, groups, 'lab');
		expect(rs.map((r) => r.id)).toEqual(['a', 'b']);
		expect(gs.map((g) => g.path)).toEqual(['lab']);
	});

	it('returns empty runners for a group with no members, and no backing res for an unknown alias', () => {
		expect(filterFleetByGroup(runners, groups, 'ghost')).toEqual({
			runners: [],
			groupResources: []
		});
	});

	it('treats an absent group field as ungrouped (never matched by a real alias)', () => {
		const { runners: rs } = filterFleetByGroup([runner('x', null)], [], 'lab');
		expect(rs).toEqual([]);
	});
});
