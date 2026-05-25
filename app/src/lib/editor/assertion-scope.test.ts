import { describe, expect, it } from 'vitest';
import type { components } from '$lib/api/schema';

import { buildAssertionScope } from './assertion-scope';

type WorkflowGraph = components['schemas']['WorkflowGraph'];
type WorkflowNode = components['schemas']['WorkflowNode'];

function endNode(
	id: string,
	label: string,
	mappings: { target: string; expr?: string }[]
): WorkflowNode {
	return {
		id,
		type: 'end',
		position: { x: 0, y: 0 },
		data: {
			type: 'end',
			label,
			resultMapping: mappings.map((m) => ({
				targetField: m.target,
				expression: m.expr ?? 'token'
			}))
		}
	} as WorkflowNode;
}

function automatedNode(id: string, slug: string, label: string): WorkflowNode {
	return {
		id,
		type: 'automated_step',
		slug,
		position: { x: 0, y: 0 },
		data: {
			type: 'automated_step',
			label,
			executionSpec: {
				backendType: 'python',
				entrypoint: 'main.py',
				config: {
					python: 'python3',
					requirements: [],
					virtualenv: false,
					sdk: true,
					inherit_env: true,
					env: {}
				}
			},
			retryPolicy: { maxRetries: 0, strategy: { type: 'immediate' } },
			deploymentModel: { mode: 'inline' },
			output: { id: 'out', label: 'Out', fields: [] }
		}
	} as WorkflowNode;
}

function graph(nodes: WorkflowNode[]): WorkflowGraph {
	return {
		nodes,
		edges: [],
		definitions: {},
		instanceConcurrency: { policy: 'unlimited' }
	} as WorkflowGraph;
}

describe('buildAssertionScope', () => {
	it('returns no entries when no End node has resultMapping', () => {
		const g = graph([endNode('e1', 'Done', [])]);
		expect(buildAssertionScope(g)).toEqual([]);
	});

	it('emits one entry per resultMapping field, qualified as result.value.<field>', () => {
		const g = graph([
			endNode('e1', 'Done', [{ target: 'amount' }, { target: 'approved' }])
		]);
		const entries = buildAssertionScope(g);
		expect(entries.map((e) => e.qualified)).toEqual([
			'result.value.amount',
			'result.value.approved'
		]);
	});

	it('uses the End label as the group label when there is only one End', () => {
		const g = graph([endNode('e1', 'Done', [{ target: 'amount' }])]);
		const [entry] = buildAssertionScope(g);
		expect(entry.nodeLabel).toBe('Done');
	});

	it('prefixes "End:" on group labels when there are multiple Ends', () => {
		const g = graph([
			endNode('e1', 'Approved', [{ target: 'amount' }]),
			endNode('e2', 'Rejected', [{ target: 'reason' }])
		]);
		const entries = buildAssertionScope(g);
		const groups = new Set(entries.map((e) => e.nodeLabel));
		expect(groups).toEqual(new Set(['End: Approved', 'End: Rejected']));
	});

	it('falls back to "End" when an End node has no label', () => {
		const g = graph([
			{ ...endNode('e1', '', [{ target: 'amount' }]) },
			endNode('e2', 'Rejected', [{ target: 'reason' }])
		]);
		const entries = buildAssertionScope(g);
		const groups = new Set(entries.map((e) => e.nodeLabel));
		expect(groups).toEqual(new Set(['End: End', 'End: Rejected']));
	});

	it('skips non-End nodes entirely (AutomatedStep outputs are out of scope here)', () => {
		const g = graph([
			automatedNode('a1', 'extract', 'Extract'),
			endNode('e1', 'Done', [{ target: 'amount' }])
		]);
		const entries = buildAssertionScope(g);
		expect(entries.map((e) => e.qualified)).toEqual(['result.value.amount']);
	});
});
