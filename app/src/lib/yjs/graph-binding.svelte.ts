import * as Y from 'yjs';
import type {
	WorkflowGraph,
	WorkflowNodeData,
	WorkflowNodeType,
	WorkflowEdge,
	WorkflowEdgeType
} from '$lib/types/editor';

/**
 * YjsGraphBinding observes a Y.Doc and exposes a reactive WorkflowGraph.
 *
 * Y.Doc schema:
 *   Y.Map("meta")     ← { name, description, author_id }
 *   Y.Map("nodes")    ← keyed by nodeId → Y.Map { type, label, description, config (Y.Map), position, files (Y.Map → Y.Text), parentId?, width?, height? }
 *   Y.Array("edges")  ← [{ id, source, target, sourceHandle?, label?, type }]
 *   Y.Map("viewport") ← { x, y, zoom }
 */
export class YjsGraphBinding {
	private doc: Y.Doc;
	private yNodes: Y.Map<Y.Map<unknown>>;
	private yEdges: Y.Array<Record<string, unknown>>;
	private yMeta: Y.Map<unknown>;
	private yViewport: Y.Map<number>;

	private nodesObserver: () => void;
	private edgesObserver: () => void;
	private viewportObserver: () => void;
	private deepObservers: Map<string, () => void> = new Map();

	graph: WorkflowGraph = $state({ nodes: [], edges: [] });

	constructor(doc: Y.Doc) {
		this.doc = doc;
		this.yNodes = doc.getMap('nodes') as Y.Map<Y.Map<unknown>>;
		this.yEdges = doc.getArray('edges') as Y.Array<Record<string, unknown>>;
		this.yMeta = doc.getMap('meta');
		this.yViewport = doc.getMap('viewport') as Y.Map<number>;

		this.nodesObserver = () => this.rematerialize();
		this.edgesObserver = () => this.rematerialize();
		this.viewportObserver = () => this.rematerialize();

		this.yNodes.observe(this.nodesObserver);
		this.yEdges.observe(this.edgesObserver);
		this.yViewport.observe(this.viewportObserver);

		// Deep-observe each existing node map for sub-key changes
		this.yNodes.forEach((_value, key) => {
			this.observeNodeMap(key);
		});

		// When nodes are added/removed, manage per-node deep observers
		this.yNodes.observe((event) => {
			for (const [key, change] of event.changes.keys) {
				if (change.action === 'add') {
					this.observeNodeMap(key);
				} else if (change.action === 'delete') {
					this.unobserveNodeMap(key);
				}
			}
		});

		// Initial materialization
		this.rematerialize();
	}

	private observeNodeMap(nodeId: string): void {
		const nodeMap = this.yNodes.get(nodeId);
		if (!nodeMap || !(nodeMap instanceof Y.Map)) return;

		const handler = () => this.rematerialize();
		nodeMap.observeDeep(handler);
		this.deepObservers.set(nodeId, () => nodeMap.unobserveDeep(handler));
	}

	private unobserveNodeMap(nodeId: string): void {
		const cleanup = this.deepObservers.get(nodeId);
		if (cleanup) {
			cleanup();
			this.deepObservers.delete(nodeId);
		}
	}

	private rematerialize(): void {
		const nodes: WorkflowGraph['nodes'] = [];

		this.yNodes.forEach((yNode, id) => {
			if (!(yNode instanceof Y.Map)) return;

			const type = yNode.get('type') as WorkflowNodeType;
			const posRaw = yNode.get('position') as { x: number; y: number } | undefined;
			const position = posRaw ?? { x: 0, y: 0 };

			const data = this.materializeNodeData(yNode, type);
			const parentId = yNode.get('parentId') as string | undefined;
			const width = yNode.get('width') as number | undefined;
			const height = yNode.get('height') as number | undefined;

			nodes.push({
				id,
				type,
				position,
				data,
				...(parentId ? { parentId } : {}),
				...(width != null ? { width } : {}),
				...(height != null ? { height } : {})
			});
		});

		const edges: WorkflowEdge[] = [];
		this.yEdges.forEach((item) => {
			edges.push({
				id: item.id as string,
				source: item.source as string,
				target: item.target as string,
				sourceHandle: item.sourceHandle as string | undefined,
				label: item.label as string | undefined,
				type: (item.type as WorkflowEdgeType) ?? 'sequence'
			});
		});

		const vx = this.yViewport.get('x');
		const vy = this.yViewport.get('y');
		const vz = this.yViewport.get('zoom');
		const viewport =
			vx != null && vy != null && vz != null ? { x: vx, y: vy, zoom: vz } : undefined;

		this.graph = { nodes, edges, viewport };
	}

	private materializeNodeData(yNode: Y.Map<unknown>, type: WorkflowNodeType): WorkflowNodeData {
		const label = (yNode.get('label') as string) ?? '';
		const description = yNode.get('description') as string | undefined;
		const base = { label, ...(description ? { description } : {}) };

		const configMap = yNode.get('config');
		const config =
			configMap instanceof Y.Map ? Object.fromEntries(configMap.entries()) : undefined;

		switch (type) {
			case 'start':
				return {
					...base,
					type: 'start',
					...(config?.initialData
						? { initialData: config.initialData as Record<string, unknown> }
						: {})
				};
			case 'end':
				return { ...base, type: 'end' };
			case 'human_task':
				return {
					...base,
					type: 'human_task',
					taskTitle: (config?.taskTitle as string) ?? '',
					instructionsMdsvex: config?.instructionsMdsvex as string | undefined,
					steps: (config?.steps as WorkflowNodeData extends { steps: infer S } ? S : never) ?? []
				};
			case 'automated_step':
				return {
					...base,
					type: 'automated_step',
					executionSpec: (config?.executionSpec as {
						backendType: 'python';
						config: Record<string, unknown>;
					}) ?? { backendType: 'python', config: {} }
				};
			case 'decision':
				return {
					...base,
					type: 'decision',
					conditions: (config?.conditions as { edgeId: string; label: string; guard: string }[]) ?? [],
					...(config?.defaultBranch
						? { defaultBranch: config.defaultBranch as string }
						: {})
				};
			case 'parallel_split':
				return { ...base, type: 'parallel_split' };
			case 'parallel_join':
				return { ...base, type: 'parallel_join' };
			case 'loop':
				return {
					...base,
					type: 'loop',
					maxIterations: (config?.maxIterations as number) ?? 3,
					loopCondition: (config?.loopCondition as string) ?? 'true'
				};
			case 'scope':
				return { ...base, type: 'scope' };
		}
	}

	// --- Mutation methods ---

	addNode(
		id: string,
		type: WorkflowNodeType,
		position: { x: number; y: number },
		data: WorkflowNodeData,
		opts?: { parentId?: string; width?: number; height?: number }
	): void {
		this.doc.transact(() => {
			const yNode = new Y.Map<unknown>();
			yNode.set('type', type);
			yNode.set('position', position);
			yNode.set('label', data.label);
			if (data.description) yNode.set('description', data.description);
			if (opts?.parentId) yNode.set('parentId', opts.parentId);
			if (opts?.width != null) yNode.set('width', opts.width);
			if (opts?.height != null) yNode.set('height', opts.height);

			// Store type-specific fields in config Y.Map
			const config = new Y.Map<unknown>();
			this.writeDataToConfig(config, data);
			yNode.set('config', config);

			// Files map (empty initially)
			yNode.set('files', new Y.Map<Y.Text>());

			this.yNodes.set(id, yNode);
		});
	}

	removeNode(id: string): void {
		this.doc.transact(() => {
			this.yNodes.delete(id);

			// Remove all edges connected to this node
			const toRemove: number[] = [];
			this.yEdges.forEach((edge, i) => {
				if (edge.source === id || edge.target === id) {
					toRemove.push(i);
				}
			});
			// Delete in reverse order to preserve indices
			for (let i = toRemove.length - 1; i >= 0; i--) {
				this.yEdges.delete(toRemove[i], 1);
			}
		});
	}

	updateNodeData(nodeId: string, data: Partial<WorkflowNodeData> & { type: WorkflowNodeType }): void {
		this.doc.transact(() => {
			const yNode = this.yNodes.get(nodeId);
			if (!yNode || !(yNode instanceof Y.Map)) return;

			if ('label' in data && data.label != null) yNode.set('label', data.label);
			if ('description' in data) yNode.set('description', data.description);

			let config = yNode.get('config');
			if (!(config instanceof Y.Map)) {
				config = new Y.Map<unknown>();
				yNode.set('config', config);
			}
			this.writeDataToConfig(config as Y.Map<unknown>, data as WorkflowNodeData);
		});
	}

	updateNodePosition(nodeId: string, position: { x: number; y: number }): void {
		this.doc.transact(() => {
			const yNode = this.yNodes.get(nodeId);
			if (!yNode || !(yNode instanceof Y.Map)) return;
			yNode.set('position', position);
		});
	}

	addEdge(edge: WorkflowEdge): void {
		this.doc.transact(() => {
			const obj: Record<string, unknown> = {
				id: edge.id,
				source: edge.source,
				target: edge.target,
				type: edge.type
			};
			if (edge.sourceHandle) obj.sourceHandle = edge.sourceHandle;
			if (edge.label) obj.label = edge.label;
			this.yEdges.push([obj]);
		});
	}

	removeEdge(edgeId: string): void {
		this.doc.transact(() => {
			let index = -1;
			this.yEdges.forEach((edge, i) => {
				if (edge.id === edgeId) index = i;
			});
			if (index >= 0) {
				this.yEdges.delete(index, 1);
			}
		});
	}

	// --- File operations ---

	getNodeFiles(nodeId: string): Map<string, Y.Text> {
		const yNode = this.yNodes.get(nodeId);
		if (!yNode || !(yNode instanceof Y.Map)) return new Map();
		const files = yNode.get('files');
		if (!(files instanceof Y.Map)) return new Map();
		const result = new Map<string, Y.Text>();
		files.forEach((value: unknown, key: string) => {
			if (value instanceof Y.Text) {
				result.set(key, value);
			}
		});
		return result;
	}

	createFile(nodeId: string, filename: string, content?: string): Y.Text | null {
		const yNode = this.yNodes.get(nodeId);
		if (!yNode || !(yNode instanceof Y.Map)) return null;

		let yText: Y.Text | null = null;
		this.doc.transact(() => {
			let files = yNode.get('files');
			if (!(files instanceof Y.Map)) {
				files = new Y.Map<Y.Text>();
				yNode.set('files', files);
			}
			yText = new Y.Text(content);
			(files as Y.Map<Y.Text>).set(filename, yText!);
		});
		return yText;
	}

	deleteFile(nodeId: string, filename: string): void {
		const yNode = this.yNodes.get(nodeId);
		if (!yNode || !(yNode instanceof Y.Map)) return;

		this.doc.transact(() => {
			const files = yNode.get('files');
			if (files instanceof Y.Map) {
				files.delete(filename);
			}
		});
	}

	renameFile(nodeId: string, oldName: string, newName: string): void {
		const yNode = this.yNodes.get(nodeId);
		if (!yNode || !(yNode instanceof Y.Map)) return;
		const files = yNode.get('files');
		if (!(files instanceof Y.Map)) return;

		const yText = files.get(oldName);
		if (!(yText instanceof Y.Text)) return;

		this.doc.transact(() => {
			// Create new Y.Text with same content
			const newText = new Y.Text(yText.toString());
			(files as Y.Map<Y.Text>).set(newName, newText);
			(files as Y.Map<Y.Text>).delete(oldName);
		});
	}

	getFileText(nodeId: string, filename: string): Y.Text | null {
		const yNode = this.yNodes.get(nodeId);
		if (!yNode || !(yNode instanceof Y.Map)) return null;
		const files = yNode.get('files');
		if (!(files instanceof Y.Map)) return null;
		const yText = files.get(filename);
		return yText instanceof Y.Text ? yText : null;
	}

	// --- Viewport ---

	updateViewport(viewport: { x: number; y: number; zoom: number }): void {
		this.doc.transact(() => {
			this.yViewport.set('x', viewport.x);
			this.yViewport.set('y', viewport.y);
			this.yViewport.set('zoom', viewport.zoom);
		});
	}

	// --- Helpers ---

	private writeDataToConfig(config: Y.Map<unknown>, data: WorkflowNodeData): void {
		switch (data.type) {
			case 'start':
				if (data.initialData) config.set('initialData', data.initialData);
				break;
			case 'end':
				break;
			case 'human_task':
				config.set('taskTitle', data.taskTitle);
				if (data.instructionsMdsvex) config.set('instructionsMdsvex', data.instructionsMdsvex);
				config.set('steps', data.steps);
				break;
			case 'automated_step':
				config.set('executionSpec', data.executionSpec);
				break;
			case 'decision':
				config.set('conditions', data.conditions);
				if (data.defaultBranch) config.set('defaultBranch', data.defaultBranch);
				break;
			case 'parallel_split':
			case 'parallel_join':
				break;
			case 'loop':
				config.set('maxIterations', data.maxIterations);
				config.set('loopCondition', data.loopCondition);
				break;
			case 'scope':
				break;
		}
	}

	destroy(): void {
		this.yNodes.unobserve(this.nodesObserver);
		this.yEdges.unobserve(this.edgesObserver);
		this.yViewport.unobserve(this.viewportObserver);

		// Clean up per-node deep observers
		for (const cleanup of this.deepObservers.values()) {
			cleanup();
		}
		this.deepObservers.clear();
	}
}
