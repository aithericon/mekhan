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

	it('updateNodePosition changes position', () => {
		const data = createDefaultNodeData('start');
		binding.addNode('n1', 'start', { x: 0, y: 0 }, data);

		binding.updateNodePosition('n1', { x: 500, y: 600 });

		expect(binding.graph.nodes[0].position).toEqual({ x: 500, y: 600 });
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
			'parallel_join',
			'loop',
			'scope'
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

		binding.createFile('n1', 'script.py', 'x = 1');
		binding.deleteFile('n1', 'script.py');

		expect(binding.getNodeFiles('n1').size).toBe(0);
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
});
