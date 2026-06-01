import { describe, it, expect, beforeEach } from 'vitest';
import * as Y from 'yjs';
import { YjsGraphBinding } from './graph-binding.svelte';
import { createDefaultNodeData } from '$lib/types/editor';
import type { WorkflowNodeType, WorkflowEdge, WorkflowNodeData } from '$lib/types/editor';

describe('YjsGraphBinding', () => {
	let doc: Y.Doc;
	let binding: YjsGraphBinding;

	beforeEach(() => {
		doc = new Y.Doc();
		binding = new YjsGraphBinding(doc);
	});

	it('initial state is empty graph', () => {
		expect(binding.graph.nodes).toEqual([]);
		expect(binding.graph.edges).toEqual([]);
	});

	it('addNode materializes correctly', () => {
		const data = createDefaultNodeData('start');
		binding.addNode('n1', 'start', { x: 100, y: 200 }, data);

		expect(binding.graph.nodes).toHaveLength(1);
		const node = binding.graph.nodes[0];
		expect(node.id).toBe('n1');
		expect(node.type).toBe('start');
		expect(node.position).toEqual({ x: 100, y: 200 });
		expect(node.data.type).toBe('start');
		expect(node.data.label).toBe('Start');
	});

	it('addNode with parentId and dimensions', () => {
		const data = createDefaultNodeData('end');
		binding.addNode('child1', 'end', { x: 50, y: 50 }, data, {
			parentId: 'scope1',
			width: 300,
			height: 200
		});

		const node = binding.graph.nodes[0];
		expect(node.parentId).toBe('scope1');
		expect(node.width).toBe(300);
		expect(node.height).toBe(200);
	});

	it('removeNode removes node and connected edges', () => {
		const startData = createDefaultNodeData('start');
		const htData = createDefaultNodeData('human_task');
		const endData = createDefaultNodeData('end');

		binding.addNode('n1', 'start', { x: 0, y: 0 }, startData);
		binding.addNode('n2', 'human_task', { x: 100, y: 0 }, htData);
		binding.addNode('n3', 'end', { x: 200, y: 0 }, endData);

		binding.addEdge({ id: 'e1', source: 'n1', target: 'n2', type: 'sequence' });
		binding.addEdge({ id: 'e2', source: 'n2', target: 'n3', type: 'sequence' });

		binding.removeNode('n2');

		expect(binding.graph.nodes).toHaveLength(2);
		expect(binding.graph.nodes.find((n) => n.id === 'n2')).toBeUndefined();
		expect(binding.graph.edges).toHaveLength(0);
	});

	it('updateNodeData changes properties', () => {
		const data = createDefaultNodeData('human_task');
		binding.addNode('n1', 'human_task', { x: 0, y: 0 }, data);

		binding.updateNodeData('n1', {
			type: 'human_task',
			label: 'Review Form',
			taskTitle: 'Please review the document'
		});

		const node = binding.graph.nodes[0];
		expect(node.data.label).toBe('Review Form');
		expect(node.data.type).toBe('human_task');
		if (node.data.type === 'human_task') {
			expect(node.data.taskTitle).toBe('Please review the document');
		}
	});

	it('updateNodeData prunes decision edges wired to removed branch handles', () => {
		const decisionData = {
			...createDefaultNodeData('decision'),
			conditions: [
				{ edgeId: 'branch-a', label: 'A', guard: 'true' },
				{ edgeId: 'branch-b', label: 'B', guard: 'false' }
			],
			defaultBranch: 'default'
		} as WorkflowNodeData;

		binding.addNode('d1', 'decision', { x: 0, y: 0 }, decisionData);
		binding.addNode('s1', 'start', { x: -100, y: 0 }, createDefaultNodeData('start'));
		binding.addNode('t1', 'end', { x: 100, y: 0 }, createDefaultNodeData('end'));
		binding.addNode('t2', 'end', { x: 100, y: 100 }, createDefaultNodeData('end'));
		binding.addNode('t3', 'end', { x: 100, y: 200 }, createDefaultNodeData('end'));

		binding.addEdge({
			id: 'ea',
			source: 'd1',
			target: 't1',
			type: 'conditional',
			sourceHandle: 'branch-a'
		});
		binding.addEdge({
			id: 'eb',
			source: 'd1',
			target: 't2',
			type: 'conditional',
			sourceHandle: 'branch-b'
		});
		binding.addEdge({
			id: 'edef',
			source: 'd1',
			target: 't3',
			type: 'conditional',
			sourceHandle: 'default'
		});
		// No sourceHandle: compiler falls back to the first port, so keep it.
		binding.addEdge({ id: 'enoh', source: 'd1', target: 't1', type: 'conditional' });
		// Edge into the decision node is unrelated to its output handles.
		binding.addEdge({ id: 'ein', source: 's1', target: 'd1', type: 'sequence' });

		binding.updateNodeData('d1', {
			type: 'decision',
			conditions: [{ edgeId: 'branch-a', label: 'A', guard: 'true' }],
			defaultBranch: 'default'
		} as WorkflowNodeData);

		expect(binding.graph.edges.map((e) => e.id).sort()).toEqual([
			'ea',
			'edef',
			'ein',
			'enoh'
		]);
	});

	it('reordering decision conditions swaps order and keeps wired edges', () => {
		const decisionData = {
			...createDefaultNodeData('decision'),
			conditions: [
				{ edgeId: 'branch-a', label: 'A', guard: 'g0' },
				{ edgeId: 'branch-b', label: 'B', guard: 'g1' },
				{ edgeId: 'branch-c', label: 'C', guard: 'g2' }
			],
			defaultBranch: 'default'
		} as WorkflowNodeData;

		binding.addNode('d1', 'decision', { x: 0, y: 0 }, decisionData);
		binding.addNode('t1', 'end', { x: 100, y: 0 }, createDefaultNodeData('end'));
		binding.addNode('t2', 'end', { x: 100, y: 100 }, createDefaultNodeData('end'));

		binding.addEdge({
			id: 'ea',
			source: 'd1',
			target: 't1',
			type: 'conditional',
			sourceHandle: 'branch-a'
		});
		binding.addEdge({
			id: 'ec',
			source: 'd1',
			target: 't2',
			type: 'conditional',
			sourceHandle: 'branch-c'
		});

		// Move 'C' to the top (the move-up control applied twice == these
		// array swaps the UI performs).
		binding.updateNodeData('d1', {
			type: 'decision',
			conditions: [
				{ edgeId: 'branch-c', label: 'C', guard: 'g2' },
				{ edgeId: 'branch-a', label: 'A', guard: 'g0' },
				{ edgeId: 'branch-b', label: 'B', guard: 'g1' }
			],
			defaultBranch: 'default'
		} as WorkflowNodeData);

		const node = binding.graph.nodes.find((n) => n.id === 'd1')!;
		expect(node.data.type).toBe('decision');
		if (node.data.type === 'decision') {
			expect(node.data.conditions.map((c) => c.edgeId)).toEqual([
				'branch-c',
				'branch-a',
				'branch-b'
			]);
		}

		// Edge wiring is keyed by the stable edgeId, so a reorder must not
		// drop or rewire any drawn edge.
		expect(binding.graph.edges.map((e) => e.id).sort()).toEqual(['ea', 'ec']);
		const ec = binding.graph.edges.find((e) => e.id === 'ec');
		expect(ec?.sourceHandle).toBe('branch-c');
	});

	it('updateNodeData prunes the default-branch edge when defaultBranch is disabled', () => {
		const decisionData = {
			...createDefaultNodeData('decision'),
			conditions: [{ edgeId: 'branch-a', label: 'A', guard: 'true' }],
			defaultBranch: 'default'
		} as WorkflowNodeData;

		binding.addNode('d1', 'decision', { x: 0, y: 0 }, decisionData);
		binding.addNode('t1', 'end', { x: 100, y: 0 }, createDefaultNodeData('end'));
		binding.addNode('t2', 'end', { x: 100, y: 100 }, createDefaultNodeData('end'));

		binding.addEdge({
			id: 'ea',
			source: 'd1',
			target: 't1',
			type: 'conditional',
			sourceHandle: 'branch-a'
		});
		binding.addEdge({
			id: 'edef',
			source: 'd1',
			target: 't2',
			type: 'conditional',
			sourceHandle: 'default'
		});

		binding.updateNodeData('d1', {
			type: 'decision',
			conditions: [{ edgeId: 'branch-a', label: 'A', guard: 'true' }]
		} as WorkflowNodeData);

		expect(binding.graph.edges.map((e) => e.id)).toEqual(['ea']);
	});

	it('updateNodePosition changes position', () => {
		const data = createDefaultNodeData('start');
		binding.addNode('n1', 'start', { x: 0, y: 0 }, data);

		binding.updateNodePosition('n1', { x: 500, y: 600 });

		expect(binding.graph.nodes[0].position).toEqual({ x: 500, y: 600 });
	});

	it('resizeNode persists width/height (and optional position)', () => {
		const data = createDefaultNodeData('scope');
		binding.addNode('s1', 'scope', { x: 10, y: 20 }, data, {
			width: 400,
			height: 200
		});

		// Bottom-right resize: size only.
		binding.resizeNode('s1', { width: 520, height: 260 });
		let node = binding.graph.nodes[0];
		expect(node.width).toBe(520);
		expect(node.height).toBe(260);
		expect(node.position).toEqual({ x: 10, y: 20 });

		// Top-left resize: position shifts with size.
		binding.resizeNode('s1', {
			position: { x: 5, y: 15 },
			width: 600,
			height: 300
		});
		node = binding.graph.nodes[0];
		expect(node.position).toEqual({ x: 5, y: 15 });
		expect(node.width).toBe(600);
		expect(node.height).toBe(300);
	});

	it('addEdge appends to edges', () => {
		const edge: WorkflowEdge = {
			id: 'e1',
			source: 'n1',
			target: 'n2',
			type: 'sequence'
		};
		binding.addEdge(edge);

		expect(binding.graph.edges).toHaveLength(1);
		expect(binding.graph.edges[0].id).toBe('e1');
		expect(binding.graph.edges[0].source).toBe('n1');
		expect(binding.graph.edges[0].target).toBe('n2');
		expect(binding.graph.edges[0].type).toBe('sequence');
	});

	it('removeEdge removes specific edge', () => {
		binding.addEdge({ id: 'e1', source: 'n1', target: 'n2', type: 'sequence' });
		binding.addEdge({ id: 'e2', source: 'n2', target: 'n3', type: 'conditional' });

		binding.removeEdge('e1');

		expect(binding.graph.edges).toHaveLength(1);
		expect(binding.graph.edges[0].id).toBe('e2');
	});

	it('all node types materialize correctly', () => {
		const allTypes: WorkflowNodeType[] = [
			'start',
			'end',
			'human_task',
			'automated_step',
			'decision',
			'parallel_split',
			'join',
			'loop',
			'scope',
			'phase_update',
			'progress_update',
			'failure'
		];

		for (const type of allTypes) {
			const data = createDefaultNodeData(type);
			binding.addNode(`node-${type}`, type, { x: 0, y: 0 }, data);
		}

		expect(binding.graph.nodes).toHaveLength(allTypes.length);

		for (const type of allTypes) {
			const node = binding.graph.nodes.find((n) => n.id === `node-${type}`);
			expect(node, `node of type ${type} should exist`).toBeDefined();
			expect(node!.data.type).toBe(type);
		}

		// Verify type-specific fields
		const htNode = binding.graph.nodes.find((n) => n.data.type === 'human_task');
		expect(htNode).toBeDefined();
		if (htNode?.data.type === 'human_task') {
			expect(htNode.data.taskTitle).toBe('New Task');
			expect(htNode.data.steps).toHaveLength(1);
		}

		const loopNode = binding.graph.nodes.find((n) => n.data.type === 'loop');
		expect(loopNode).toBeDefined();
		if (loopNode?.data.type === 'loop') {
			expect(loopNode.data.maxIterations).toBe(3);
			expect(loopNode.data.loopCondition).toBe('true');
		}

		const autoNode = binding.graph.nodes.find((n) => n.data.type === 'automated_step');
		expect(autoNode).toBeDefined();
		if (autoNode?.data.type === 'automated_step') {
			expect(autoNode.data.executionSpec.backendType).toBe('python');
		}

		const decisionNode = binding.graph.nodes.find((n) => n.data.type === 'decision');
		expect(decisionNode).toBeDefined();
		if (decisionNode?.data.type === 'decision') {
			expect(decisionNode.data.conditions).toEqual([]);
		}

		const phaseNode = binding.graph.nodes.find((n) => n.data.type === 'phase_update');
		expect(phaseNode).toBeDefined();
		if (phaseNode?.data.type === 'phase_update') {
			expect(phaseNode.data.phaseName).toBe('New phase');
			expect(phaseNode.data.status).toBe('running');
		}

		const progressNode = binding.graph.nodes.find((n) => n.data.type === 'progress_update');
		expect(progressNode).toBeDefined();
		if (progressNode?.data.type === 'progress_update') {
			expect(progressNode.data.fraction).toBe(0);
		}

		const failureNode = binding.graph.nodes.find((n) => n.data.type === 'failure');
		expect(failureNode).toBeDefined();
		if (failureNode?.data.type === 'failure') {
			expect(failureNode.data.label).toBe('Failure');
		}
	});

	it('automated_step output port survives the updateNodeData round-trip', () => {
		// Regression: the editor "Add field" on an automated step (Python is the
		// default backend) calls updateNodeData with the new `output` port. The
		// Yjs binding must persist AND re-materialize `output`, otherwise the
		// added field is dropped on write and the panel snaps back to empty —
		// i.e. "I can't add outputs" on the Python automated node.
		const data = createDefaultNodeData('automated_step');
		binding.addNode('n1', 'automated_step', { x: 0, y: 0 }, data);

		const node = binding.graph.nodes.find((n) => n.id === 'n1');
		expect(node?.data.type).toBe('automated_step');
		if (node?.data.type !== 'automated_step') return;

		binding.updateNodeData('n1', {
			...node.data,
			output: {
				id: 'out',
				label: 'Output',
				fields: [{ name: 'result', label: 'Result', kind: 'json', required: false }]
			}
		} as Extract<WorkflowNodeData, { type: 'automated_step' }>);

		const after = binding.graph.nodes.find((n) => n.id === 'n1');
		expect(after?.data.type).toBe('automated_step');
		if (after?.data.type !== 'automated_step') return;
		expect(after.data.output).toBeDefined();
		expect(after.data.output?.fields).toEqual([
			{ name: 'result', label: 'Result', kind: 'json', required: false }
		]);
	});

	it('createFile + getNodeFiles + getFileText', () => {
		const data = createDefaultNodeData('automated_step');
		binding.addNode('n1', 'automated_step', { x: 0, y: 0 }, data);

		binding.createFile('n1', 'main.py', 'print("hello")');

		const files = binding.getNodeFiles('n1');
		expect(files.size).toBe(1);
		expect(files.has('main.py')).toBe(true);

		const text = binding.getFileText('n1', 'main.py');
		expect(text).not.toBeNull();
		expect(text!.toString()).toBe('print("hello")');
	});

	it('deleteFile removes file', () => {
		const data = createDefaultNodeData('automated_step');
		binding.addNode('n1', 'automated_step', { x: 0, y: 0 }, data);

		// Python automated_step seeds `main.py` at addNode time, so the file
		// we delete must be a different filename and the post-delete size
		// reflects only the seed.
		binding.createFile('n1', 'script.py', 'x = 1');
		expect(binding.getNodeFiles('n1').size).toBe(2);
		binding.deleteFile('n1', 'script.py');

		expect(binding.getNodeFiles('n1').size).toBe(1);
		expect(binding.getNodeFiles('n1').has('main.py')).toBe(true);
		expect(binding.getFileText('n1', 'script.py')).toBeNull();
	});

	it('renameFile preserves content', () => {
		const data = createDefaultNodeData('automated_step');
		binding.addNode('n1', 'automated_step', { x: 0, y: 0 }, data);

		binding.createFile('n1', 'old.py', 'content = True');
		binding.renameFile('n1', 'old.py', 'new.py');

		expect(binding.getFileText('n1', 'old.py')).toBeNull();

		const newText = binding.getFileText('n1', 'new.py');
		expect(newText).not.toBeNull();
		expect(newText!.toString()).toBe('content = True');
	});

	it('file ops return empty for nonexistent node', () => {
		const files = binding.getNodeFiles('fake');
		expect(files.size).toBe(0);
	});

	it('updateViewport sets viewport', () => {
		binding.updateViewport({ x: 10, y: 20, zoom: 1.5 });

		expect(binding.graph.viewport).toEqual({ x: 10, y: 20, zoom: 1.5 });
	});

	it('destroy unsubscribes observers', () => {
		const data = createDefaultNodeData('start');
		binding.addNode('n1', 'start', { x: 0, y: 0 }, data);
		expect(binding.graph.nodes).toHaveLength(1);

		binding.destroy();

		// Mutate doc directly — binding should NOT update
		doc.transact(() => {
			const yNodes = doc.getMap('nodes') as Y.Map<Y.Map<unknown>>;
			const yNode = new Y.Map<unknown>();
			yNode.set('type', 'end');
			yNode.set('position', { x: 0, y: 0 });
			yNode.set('label', 'End');
			yNode.set('config', new Y.Map());
			yNode.set('files', new Y.Map());
			yNodes.set('n2', yNode);
		});

		// binding.graph should still have only the original node
		expect(binding.graph.nodes).toHaveLength(1);
	});

	it('two bindings on same doc share state', () => {
		const bindingB = new YjsGraphBinding(doc);

		const data = createDefaultNodeData('start');
		binding.addNode('n1', 'start', { x: 0, y: 0 }, data);

		expect(bindingB.graph.nodes).toHaveLength(1);
		expect(bindingB.graph.nodes[0].id).toBe('n1');

		bindingB.destroy();
	});

	it('edge with optional fields', () => {
		const edge: WorkflowEdge = {
			id: 'e1',
			source: 'n1',
			target: 'n2',
			type: 'conditional',
			sourceHandle: 'branch-a',
			label: 'Yes'
		};
		binding.addEdge(edge);

		const materialized = binding.graph.edges[0];
		expect(materialized.sourceHandle).toBe('branch-a');
		expect(materialized.label).toBe('Yes');
		expect(materialized.type).toBe('conditional');
	});

	it('phase_update config survives the updateNodeData round-trip', () => {
		// Exercises materializeNodeData ⇄ writeDataToConfig for the new control
		// node: phaseName/status/message must persist into the Yjs config map
		// and re-materialize, and clearing an optional message must delete the
		// key (not leave a stale value).
		binding.addNode('p1', 'phase_update', { x: 0, y: 0 }, createDefaultNodeData('phase_update'));

		const node = binding.graph.nodes.find((n) => n.id === 'p1');
		expect(node?.data.type).toBe('phase_update');
		if (node?.data.type !== 'phase_update') return;

		binding.updateNodeData('p1', {
			...node.data,
			phaseName: 'Validation',
			status: 'completed',
			message: 'done {{ invoice_id }}'
		} as Extract<WorkflowNodeData, { type: 'phase_update' }>);

		let after = binding.graph.nodes.find((n) => n.id === 'p1');
		expect(after?.data.type).toBe('phase_update');
		if (after?.data.type !== 'phase_update') return;
		expect(after.data.phaseName).toBe('Validation');
		expect(after.data.status).toBe('completed');
		expect(after.data.message).toBe('done {{ invoice_id }}');

		binding.updateNodeData('p1', {
			...after.data,
			message: undefined
		} as Extract<WorkflowNodeData, { type: 'phase_update' }>);
		after = binding.graph.nodes.find((n) => n.id === 'p1');
		if (after?.data.type !== 'phase_update') return;
		expect(after.data.message).toBeUndefined();
	});

	it('progress_update config survives the updateNodeData round-trip', () => {
		binding.addNode(
			'g1',
			'progress_update',
			{ x: 0, y: 0 },
			createDefaultNodeData('progress_update')
		);

		const node = binding.graph.nodes.find((n) => n.id === 'g1');
		expect(node?.data.type).toBe('progress_update');
		if (node?.data.type !== 'progress_update') return;

		binding.updateNodeData('g1', {
			...node.data,
			fraction: 0.5,
			message: 'processed {{ count }}',
			currentStep: 2,
			totalSteps: 5
		} as Extract<WorkflowNodeData, { type: 'progress_update' }>);

		let after = binding.graph.nodes.find((n) => n.id === 'g1');
		expect(after?.data.type).toBe('progress_update');
		if (after?.data.type !== 'progress_update') return;
		expect(after.data.fraction).toBe(0.5);
		expect(after.data.message).toBe('processed {{ count }}');
		expect(after.data.currentStep).toBe(2);
		expect(after.data.totalSteps).toBe(5);

		binding.updateNodeData('g1', {
			...after.data,
			currentStep: undefined,
			totalSteps: undefined,
			message: undefined
		} as Extract<WorkflowNodeData, { type: 'progress_update' }>);
		after = binding.graph.nodes.find((n) => n.id === 'g1');
		if (after?.data.type !== 'progress_update') return;
		expect(after.data.currentStep).toBeUndefined();
		expect(after.data.totalSteps).toBeUndefined();
		expect(after.data.message).toBeUndefined();
		expect(after.data.fraction).toBe(0.5);
	});

	it('failure config survives the updateNodeData round-trip', () => {
		// Exercises materializeNodeData ⇄ writeDataToConfig for the Failure
		// node: failureMessage must persist + re-materialize, and clearing it
		// must delete the key (not leave a stale value).
		binding.addNode('f1', 'failure', { x: 0, y: 0 }, createDefaultNodeData('failure'));

		const node = binding.graph.nodes.find((n) => n.id === 'f1');
		expect(node?.data.type).toBe('failure');
		if (node?.data.type !== 'failure') return;

		binding.updateNodeData('f1', {
			...node.data,
			failureMessage: 'failed for {{ order_id }}'
		} as Extract<WorkflowNodeData, { type: 'failure' }>);

		let after = binding.graph.nodes.find((n) => n.id === 'f1');
		expect(after?.data.type).toBe('failure');
		if (after?.data.type !== 'failure') return;
		expect(after.data.failureMessage).toBe('failed for {{ order_id }}');

		binding.updateNodeData('f1', {
			...after.data,
			failureMessage: undefined
		} as Extract<WorkflowNodeData, { type: 'failure' }>);
		after = binding.graph.nodes.find((n) => n.id === 'f1');
		if (after?.data.type !== 'failure') return;
		expect(after.data.failureMessage).toBeUndefined();
	});
});
