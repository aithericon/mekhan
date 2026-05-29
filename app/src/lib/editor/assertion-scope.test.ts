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

function automatedNode(
	id: string,
	slug: string,
	label: string,
	outputFields: string[] = []
): WorkflowNode {
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
			deploymentModel: { mode: 'executor' },
			output: {
				id: 'out',
				label: 'Out',
				fields: outputFields.map((name) => ({
					name,
					label: name,
					kind: 'text',
					required: true
				}))
			}
		}
	} as WorkflowNode;
}

function startNode(id: string, label: string, fields: string[] = []): WorkflowNode {
	return {
		id,
		type: 'start',
		position: { x: 0, y: 0 },
		data: {
			type: 'start',
			label,
			initial: {
				id: 'in',
				label: 'In',
				fields: fields.map((name) => ({
					name,
					label: name,
					kind: 'text',
					required: true
				}))
			}
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
	it('returns no entries when no node contributes a path', () => {
		const g = graph([endNode('e1', 'Done', []), startNode('s1', 'Start')]);
		expect(buildAssertionScope(g)).toEqual([]);
	});

	it('emits End resultMapping fields as result.value.<field>', () => {
		const g = graph([
			endNode('e1', 'Done', [{ target: 'amount' }, { target: 'approved' }])
		]);
		expect(buildAssertionScope(g).map((e) => e.qualified)).toEqual([
			'result.value.amount',
			'result.value.approved'
		]);
	});

	it('emits AutomatedStep output fields as steps.<slug>.output.<field>', () => {
		const g = graph([automatedNode('a1', 'extract', 'Extract', ['vendor', 'amount'])]);
		expect(buildAssertionScope(g).map((e) => e.qualified)).toEqual([
			'steps.extract.output.vendor',
			'steps.extract.output.amount'
		]);
	});

	it('hoists Start initial fields to start.<field> when there is only one Start', () => {
		const g = graph([startNode('start', 'Start', ['invoice_id', 'amount'])]);
		expect(buildAssertionScope(g).map((e) => e.qualified)).toEqual([
			'start.invoice_id',
			'start.amount'
		]);
	});

	it('namespaces Start fields under <block_id> when there are multiple Starts', () => {
		const g = graph([
			startNode('manual', 'Manual', ['a']),
			startNode('trigger', 'Trigger', ['b'])
		]);
		expect(new Set(buildAssertionScope(g).map((e) => e.qualified))).toEqual(
			new Set(['start.manual.a', 'start.trigger.b'])
		);
	});

	it('uses the End label as the group label when there is only one End', () => {
		const g = graph([endNode('e1', 'Done', [{ target: 'amount' }])]);
		expect(buildAssertionScope(g)[0].nodeLabel).toBe('Done');
	});

	it('prefixes "End:" / "Start:" only when multiple of that kind exist', () => {
		const g = graph([
			startNode('s1', 'Manual', ['a']),
			startNode('s2', 'Trigger', ['b']),
			endNode('e1', 'Approved', [{ target: 'x' }]),
			endNode('e2', 'Rejected', [{ target: 'y' }])
		]);
		const groups = new Set(buildAssertionScope(g).map((e) => e.nodeLabel));
		expect(groups).toEqual(
			new Set(['Start: Manual', 'Start: Trigger', 'End: Approved', 'End: Rejected'])
		);
	});

	it('combines End + AutomatedStep + Start entries in one scope', () => {
		const g = graph([
			startNode('start', 'Start', ['amount']),
			automatedNode('a1', 'review', 'Review', ['approved']),
			endNode('e1', 'Done', [{ target: 'amount' }])
		]);
		const quals = buildAssertionScope(g).map((e) => e.qualified);
		expect(new Set(quals)).toEqual(
			new Set([
				// Single Start → hoisted to `start.<field>`.
				'start.amount',
				'steps.review.output.approved',
				'result.value.amount'
			])
		);
	});

	it('falls back to node.id when an AutomatedStep has no slug', () => {
		const node = automatedNode('a1', '', 'Extract', ['x']);
		const g = graph([node]);
		// The helper falls back to node.id when slug is blank/missing.
		expect(buildAssertionScope(g)[0].qualified).toBe('steps.a1.output.x');
	});
});
