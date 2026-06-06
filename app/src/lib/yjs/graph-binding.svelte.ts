import * as Y from 'yjs';
import type {
	WorkflowGraph,
	WorkflowNodeData,
	WorkflowNodeType,
	WorkflowEdge,
	WorkflowEdgeType,
	TriggerNodeData,
	PhaseUpdateNodeData,
	SubWorkflowNodeData,
	AutomatedStepNodeData,
	EndNodeData,
	FailureNodeData,
	AgentNodeData
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
			const slug = yNode.get('slug') as string | undefined;
			const parentId = yNode.get('parentId') as string | undefined;
			const width = yNode.get('width') as number | undefined;
			const height = yNode.get('height') as number | undefined;

			nodes.push({
				id,
				type,
				position,
				data,
				...(slug ? { slug } : {}),
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
				targetHandle: item.targetHandle as string | undefined,
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
			case 'start': {
				// Typed-ports model: Start carries a declared `initial` port.
				// Pre-typed-ports templates have no `initial` in Y.Doc — default
				// to the empty input port so legacy data loads unchanged.
				const initial = (config?.initial as
					| { id: string; label: string; fields?: unknown[] }
					| undefined) ?? { id: 'in', label: 'Input', fields: [] };
				const processName = config?.processName as string | undefined;
				return {
					...base,
					type: 'start',
					initial: {
						id: initial.id ?? 'in',
						label: initial.label ?? 'Input',
						fields: (initial.fields ?? []) as WorkflowNodeData extends {
							initial: { fields: infer F };
						}
							? F
							: never
					},
					...(processName ? { processName } : {})
				};
			}
			case 'end': {
				const resultMapping = config?.resultMapping as
					| EndNodeData['resultMapping']
					| undefined;
				return {
					...base,
					type: 'end',
					...(resultMapping && resultMapping.length > 0 ? { resultMapping } : {})
				};
			}
			case 'human_task':
				return {
					...base,
					type: 'human_task',
					taskTitle: (config?.taskTitle as string) ?? '',
					instructionsMdsvex: config?.instructionsMdsvex as string | undefined,
					steps: (config?.steps as WorkflowNodeData extends { steps: infer S } ? S : never) ?? [],
					// Opt-in dynamic-steps source: a `<slug>.<field>` ref to an upstream
					// producer that emits the form blocks at runtime. Undefined ⇒ static
					// `steps` authoring. (Not yet in the generated schema → cast.)
					stepsRef: config?.stepsRef as string | undefined
				} as WorkflowNodeData;
			case 'automated_step': {
				const spec = (config?.executionSpec as {
					backendType: 'python';
					entrypoint?: string;
					config: Record<string, unknown>;
				}) ?? { backendType: 'python', entrypoint: 'main.py', config: {} };
				const retryPolicy = (config?.retryPolicy as {
					maxRetries: number;
					backoff: 'immediate' | 'fixed' | 'exponential';
					baseDelayMs: number;
				}) ?? { maxRetries: 3, backoff: 'immediate', baseDelayMs: 0 };
				const output = config?.output as
					| { id: string; label: string; fields: unknown[] }
					| undefined;
				// `deploymentModel` carries the executor/scheduled split AND (post-R3
				// consolidation) the executor capacity admission under
				// `Executor.capacity` + the scheduled `scheduler`/`operation` knobs.
				// The whole nested object round-trips as one value.
				const deploymentModel = config?.deploymentModel as
					| AutomatedStepNodeData['deploymentModel']
					| undefined;
				// `channels` (docs/25) carries the node's statically-declared
				// streaming Channels — each control-output channel exposes a
				// per-name handle the job emits into at runtime. The whole array
				// round-trips as one value; it MUST be read back here or the
				// editor reconstruction drops the channels and their handles never
				// render even though the backend seeded them.
				const channels = config?.channels as
					| AutomatedStepNodeData['channels']
					| undefined;
				// `requirements` (Phase 4) carries the step's capability-match
				// constraints. The whole nested object round-trips as one value —
				// it MUST be read back (and written below) or a template authored
				// with requirements silently drops them on the next graph mutation
				// (the Yjs graph-binding drop-class trap).
				const requirements = config?.requirements as
					| AutomatedStepNodeData['requirements']
					| undefined;
				// `assetBindings` binds scope-visible assets the node stages as
				// inputs (docs/20 §5). Top-level node field → must round-trip here
				// or the editor reconstruction drops the bindings.
				const assetBindings = config?.assetBindings as
					| AutomatedStepNodeData['assetBindings']
					| undefined;
				return {
					...base,
					type: 'automated_step',
					executionSpec: { entrypoint: 'main.py', ...spec },
					...(output ? { output: output as never } : {}),
					retryPolicy,
					...(deploymentModel ? { deploymentModel } : {}),
					...(channels && channels.length > 0 ? { channels } : {}),
					...(requirements ? { requirements } : {}),
					...(assetBindings && assetBindings.length > 0 ? { assetBindings } : {})
				};
			}
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
			case 'join': {
				type JoinDataT = Extract<WorkflowNodeData, { type: 'join' }>;
				const mode = (config?.mode as 'all' | 'any') ?? 'all';
				const output =
					(config?.output as JoinDataT['output'] | undefined) ??
					({ id: 'out', label: 'Output', fields: [] } as JoinDataT['output']);
				return {
					...base,
					type: 'join',
					mode,
					...(mode === 'all'
						? {
								mergeStrategy:
									(config?.mergeStrategy as 'shallow_last_wins' | 'deep_merge') ??
									'shallow_last_wins'
							}
						: {}),
					output
				};
			}
			case 'loop':
				return {
					...base,
					type: 'loop',
					maxIterations: (config?.maxIterations as number) ?? 3,
					loopCondition: (config?.loopCondition as string) ?? 'true',
					accumulators:
						(config?.accumulators as { var: string; init: string; mergeExpr: string }[]) ?? []
				};
			case 'map': {
				type MapDataT = Extract<WorkflowNodeData, { type: 'map' }>;
				return {
					...base,
					type: 'map',
					itemsRef: (config?.itemsRef as string) ?? '',
					itemVar: (config?.itemVar as string) ?? 'item',
					resultVar: (config?.resultVar as string) ?? '',
					output:
						(config?.output as MapDataT['output'] | undefined) ??
						({ id: 'out', label: 'Element', fields: [] } as MapDataT['output'])
				};
			}
			case 'scope':
				return { ...base, type: 'scope' };
			case 'lease_scope': {
				type LeaseScopeDataT = Extract<WorkflowNodeData, { type: 'lease_scope' }>;
				return {
					...base,
					type: 'lease_scope',
					lease:
						(config?.lease as LeaseScopeDataT['lease'] | undefined) ??
						({ pool: '' } as LeaseScopeDataT['lease']),
						// Optional presence-placement Requirements (the scope picks WHICH
						// runner to hold). Carried through so editor round-trips don't drop it.
						requirements: config?.requirements as LeaseScopeDataT['requirements']
				};
			}
			case 'phase_update':
				return {
					...base,
					type: 'phase_update',
					phaseName: (config?.phaseName as string) ?? '',
					status:
						(config?.status as PhaseUpdateNodeData['status']) ?? 'running',
					...(config?.message ? { message: config.message as string } : {})
				};
			case 'progress_update':
				return {
					...base,
					type: 'progress_update',
					fraction: (config?.fraction as number) ?? 0,
					...(config?.message ? { message: config.message as string } : {}),
					...(config?.currentStep !== undefined
						? { currentStep: config.currentStep as number }
						: {}),
					...(config?.totalSteps !== undefined
						? { totalSteps: config.totalSteps as number }
						: {})
				};
			case 'failure': {
				const errorResultMapping = config?.errorResultMapping as
					| FailureNodeData['errorResultMapping']
					| undefined;
				return {
					...base,
					type: 'failure',
					...(config?.failureMessage
						? { failureMessage: config.failureMessage as string }
						: {}),
					...(errorResultMapping && errorResultMapping.length > 0
						? { errorResultMapping }
						: {})
				};
			}
			case 'trigger':
				return {
					...base,
					type: 'trigger',
					source: (config?.source as TriggerNodeData['source']) ?? {
						kind: 'manual',
						form: []
					},
					concurrency: (config?.concurrency as TriggerNodeData['concurrency']) ?? 'allow',
					payloadMapping: (config?.payloadMapping as TriggerNodeData['payloadMapping']) ?? [],
					enabled: (config?.enabled as boolean) ?? false
				};
			case 'sub_workflow':
				return {
					...base,
					type: 'sub_workflow',
					templateId: (config?.templateId as string) ?? '',
					versionPin:
						(config?.versionPin as SubWorkflowNodeData['versionPin']) ?? {
							mode: 'latest'
						},
					...(config?.inputMapping
						? {
								inputMapping:
									config.inputMapping as SubWorkflowNodeData['inputMapping']
							}
						: {}),
					// Child input-contract snapshot (display-only: drives the node-face
					// "consumes" preview). MUST round-trip — the SubWorkflowSection
					// contract effect reconciles it via onchange and compares against
					// the persisted value; if it never persists, that comparison stays
					// false forever and the effect re-fetches in an infinite loop.
					...(config?.inputContract
						? {
								inputContract:
									config.inputContract as SubWorkflowNodeData['inputContract']
							}
						: {}),
					output:
						(config?.output as SubWorkflowNodeData['output']) ?? {
							id: 'out',
							label: 'Result',
							fields: []
						}
				};
			case 'agent':
				return {
					...base,
					type: 'agent',
					model: (config?.model as AgentNodeData['model']) ?? {
						provider: 'anthropic',
						model: 'claude-haiku-4-5-20251001'
					},
					userPrompt: (config?.userPrompt as string) ?? '',
					...(config?.systemPrompt
						? { systemPrompt: config.systemPrompt as string }
						: {}),
					...(config?.responseFormat
						? { responseFormat: config.responseFormat }
						: {}),
					maxTurns: (config?.maxTurns as number) ?? 1,
					...(config?.stopWhen ? { stopWhen: config.stopWhen as string } : {}),
					contextStrategy:
						(config?.contextStrategy as AgentNodeData['contextStrategy']) ?? 'none',
					onToolError:
						(config?.onToolError as AgentNodeData['onToolError']) ?? 'feedback',
					...(() => {
						// `assetBindings` — staged-asset inputs (docs/20 §5). Top-level
						// node field → must round-trip or the editor drops the bindings.
						const ab = config?.assetBindings as AgentNodeData['assetBindings'] | undefined;
						return ab && ab.length > 0 ? { assetBindings: ab } : {};
					})()
				};
			case 'delay':
				return {
					...base,
					type: 'delay',
					durationMsExpr: (config?.durationMsExpr as string) ?? '5000'
				};
			case 'timeout':
				return {
					...base,
					type: 'timeout',
					durationMsExpr: (config?.durationMsExpr as string) ?? '60000'
				};
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

			// Files map. Seed a starter entrypoint only for Python automated_steps
			// so the node compiles before the user opens the IDE editor. Other
			// backends start empty — the FileTree shows no files until the user
			// adds them, which avoids stranded main.py files on Docker/HTTP/LLM
			// nodes.
			const files = new Y.Map<Y.Text>();
			if (
				type === 'automated_step' &&
				data.type === 'automated_step' &&
				data.executionSpec.backendType === 'python'
			) {
				files.set(
					'main.py',
					new Y.Text(
						[
							'# Python step — runs on the Aithericon executor.',
							'# Each upstream node is available as a Python global named after',
							'# its slug — no imports, no `token[...]`. Just write the access',
							'# directly: the compiler detects `<slug>.<field>` in this source',
							'# and stages the producer\'s data automatically.',
							'#',
							'#   amount = review.invoice_amount      # borrowed from upstream "review"',
							'#   vendor = review.vendor_name',
							'#',
							'# Outputs: assign declared output field names at top level — the',
							'# runner sweeps them into this node\'s output port after exec. Add',
							'# fields in the right-hand "Output" panel, then write them here:',
							'#',
							'#   result = { "ok": True }            # if "result" is declared',
							'#',
							'# Escape hatch: `set_output(name, value)` is also injected for',
							'# dynamic names or writes from inside branches/loops. Logging',
							'# helpers `log_*`, `update_progress`, `define_phases`/`update_phase`,',
							'# `log_metric`, `log_artifact` are injected too. The Reference panel',
							'# on the right lists every `<slug>.<field>` in scope at this node.',
							'',
							'log_info("step started")',
							''
						].join('\n')
					)
				);
			}
			yNode.set('files', files);

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

			// A decision node's output handles are derived from its branch
			// conditions (+ the optional default). Removing a branch deletes
			// its handle, so any edge still wired to that handle would compile
			// to CompileError::UnknownSourcePort at publish. Prune them here so
			// the graph stays consistent with the node's handle set.
			if (data.type === 'decision' && 'conditions' in data) {
				const validHandles = new Set<string>(
					(data.conditions ?? []).map((c) => c.edgeId)
				);
				if (data.defaultBranch) validHandles.add(data.defaultBranch);

				const toRemove: number[] = [];
				this.yEdges.forEach((edge, i) => {
					if (
						edge.source === nodeId &&
						typeof edge.sourceHandle === 'string' &&
						!validHandles.has(edge.sourceHandle)
					) {
						toRemove.push(i);
					}
				});
				for (let i = toRemove.length - 1; i >= 0; i--) {
					this.yEdges.delete(toRemove[i], 1);
				}
			}
		});
	}

	updateNodePosition(nodeId: string, position: { x: number; y: number }): void {
		this.doc.transact(() => {
			const yNode = this.yNodes.get(nodeId);
			if (!yNode || !(yNode instanceof Y.Map)) return;
			yNode.set('position', position);
		});
	}

	/**
	 * Resize a container node. NodeResizer fires on gesture end with the final
	 * `{x, y, width, height}` (top/left-edge resizes shift `x`/`y` as well as
	 * size, so we accept an optional `position`). One transaction so coauthors
	 * never observe a partial mid-resize state. Mirrors the size fields written
	 * at `addNode` time so the Y.Map round-trips identically.
	 */
	resizeNode(
		nodeId: string,
		change: { position?: { x: number; y: number }; width: number; height: number }
	): void {
		this.doc.transact(() => {
			const yNode = this.yNodes.get(nodeId);
			if (!yNode || !(yNode instanceof Y.Map)) return;
			if (change.position) yNode.set('position', change.position);
			yNode.set('width', change.width);
			yNode.set('height', change.height);
		});
	}

	/**
	 * Set or clear a node's container parent. Used by the drag-into-container
	 * gesture (Scope, Loop) — when a node is dropped inside a container the
	 * position passed here must already be **relative to the parent**, mirroring
	 * Svelte Flow's parent-relative child coordinates. Pass `null` to remove
	 * the parent (parent_id is dropped from the Y.Map entirely).
	 */
	setNodeParent(
		nodeId: string,
		parentId: string | null,
		position?: { x: number; y: number }
	): void {
		this.doc.transact(() => {
			const yNode = this.yNodes.get(nodeId);
			if (!yNode || !(yNode instanceof Y.Map)) return;
			if (parentId) {
				yNode.set('parentId', parentId);
			} else {
				yNode.delete('parentId');
			}
			if (position) yNode.set('position', position);
		});
	}

	/** Node-level author-facing slug — the `<slug>.<field>` guard namespace.
	 *  Empty/blank clears it (the compiler then derives a default from id). */
	updateNodeSlug(nodeId: string, slug: string): void {
		this.doc.transact(() => {
			const yNode = this.yNodes.get(nodeId);
			if (!yNode || !(yNode instanceof Y.Map)) return;
			const trimmed = slug.trim();
			if (trimmed) yNode.set('slug', trimmed);
			else yNode.delete('slug');
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
			if (edge.targetHandle) obj.targetHandle = edge.targetHandle;
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
				// Typed-ports: persist the full `initial` port shape (id, label,
				// fields). Stored as a plain object — Y.Map subkeys would help
				// multiplayer per-field edits but require richer cell wiring
				// (TODO when concurrent port editing becomes a real workflow).
				config.set('initial', data.initial);
				// Opt-in per-instance process name template. Delete the key when
				// cleared so it round-trips as "opt out" (publish reconstructs
				// the graph from this Y.Doc — an unset key must stay unset).
				if (data.processName != null && data.processName !== '') {
					config.set('processName', data.processName);
				} else {
					config.delete('processName');
				}
				break;
			case 'end':
				if (data.resultMapping && data.resultMapping.length > 0) {
					config.set('resultMapping', data.resultMapping);
				} else {
					config.delete('resultMapping');
				}
				break;
			case 'human_task':
				config.set('taskTitle', data.taskTitle);
				if (data.instructionsMdsvex) config.set('instructionsMdsvex', data.instructionsMdsvex);
				config.set('steps', data.steps);
				// Dynamic-steps source ref. Persist when set; delete to fall back to
				// static `steps` authoring. (Not yet in generated schema → cast.)
				if ((data as { stepsRef?: string }).stepsRef) {
					config.set('stepsRef', (data as { stepsRef?: string }).stepsRef);
				} else {
					config.delete('stepsRef');
				}
				break;
			case 'automated_step':
				config.set('executionSpec', data.executionSpec);
				// Declared output port. Persist whenever the editor supplies one
				// (PortsSection always sends a full Port on edit). Without this the
				// "Add field" round-trip is dropped on write and re-materializes
				// empty, so output fields can never be added.
				if (data.output) config.set('output', data.output);
				config.set(
					'retryPolicy',
					data.retryPolicy ?? { maxRetries: 3, backoff: 'immediate', baseDelayMs: 0 }
				);
				// `deploymentModel` round-trips whole — the nested `Executor.capacity`
				// (seeded/presence capacity admission) and scheduled
				// `scheduler`/`operation` knobs travel with it. Default = plain executor dispatch.
				config.set('deploymentModel', data.deploymentModel ?? { mode: 'executor' });
				// `channels` (docs/25) round-trips whole, conditionally (mirrors
				// `requirements`): persist the statically-declared streaming Channels
				// so their per-name emit handles survive the Y.Doc round-trip.
				{
					const chans = (data as AutomatedStepNodeData).channels;
					// Delete when absent/empty so clearing the last channel removes the
					// stale Yjs key (a bare `if (chans) set()` would leave it to reappear
					// on reload). Mirrors the other `config.delete(...)` clear paths.
					if (chans && chans.length > 0) config.set('channels', chans);
					else config.delete('channels');
				}
				// `requirements` (Phase 4) round-trips whole, conditionally (mirrors
				// `output`): persist when the step carries capability constraints so
				// collaborative edits don't drop them on publish.
				{
					const reqs = (data as AutomatedStepNodeData).requirements;
					// Delete when absent — clearing the last constraint emits node data
					// with `requirements` stripped, and a bare `if (reqs) set()` would
					// leave the stale key in Yjs (it would reappear on reload). Mirrors
					// the other `config.delete(...)` clear paths in this switch.
					if (reqs) config.set('requirements', reqs);
					else config.delete('requirements');
				}
				// Staged-asset bindings (docs/20 §5). Only touch the Y.Map key when
				// the incoming data EXPLICITLY carries `assetBindings` (i.e. the
				// AssetBindingsSection emitted a change). When the field is absent
				// from the data object — which happens whenever a different field
				// was updated by a handler that spread a stale snapshot that didn't
				// yet include `assetBindings` (backend-registry-before-Yjs-sync
				// race) — we preserve whatever is currently stored rather than
				// silently deleting the bindings.
				if ('assetBindings' in data) {
					if (data.assetBindings && data.assetBindings.length > 0) {
						config.set('assetBindings', data.assetBindings);
					} else {
						config.delete('assetBindings');
					}
				}
				break;
			case 'decision':
				config.set('conditions', data.conditions);
				if (data.defaultBranch) config.set('defaultBranch', data.defaultBranch);
				break;
			case 'parallel_split':
				break;
			case 'join':
				config.set('mode', data.mode ?? 'all');
				if ((data.mode ?? 'all') === 'all') {
					config.set('mergeStrategy', data.mergeStrategy ?? 'shallow_last_wins');
				} else {
					config.delete('mergeStrategy');
				}
				config.set(
					'output',
					data.output ?? { id: 'out', label: 'Output', fields: [] }
				);
				break;
			case 'loop':
				config.set('maxIterations', data.maxIterations);
				config.set('loopCondition', data.loopCondition);
				config.set('accumulators', data.accumulators ?? []);
				break;
			case 'map':
				config.set('itemsRef', data.itemsRef);
				config.set('itemVar', data.itemVar ?? 'item');
				config.set('resultVar', data.resultVar);
				config.set('output', data.output ?? { id: 'out', label: 'Element', fields: [] });
				break;
			case 'scope':
				break;
			case 'lease_scope':
				config.set('lease', data.lease);
				break;
			case 'phase_update':
				config.set('phaseName', data.phaseName);
				config.set('status', data.status ?? 'running');
				if (data.message != null && data.message !== '') {
					config.set('message', data.message);
				} else {
					config.delete('message');
				}
				break;
			case 'progress_update':
				config.set('fraction', data.fraction);
				if (data.message != null && data.message !== '') {
					config.set('message', data.message);
				} else {
					config.delete('message');
				}
				if (data.currentStep != null) {
					config.set('currentStep', data.currentStep);
				} else {
					config.delete('currentStep');
				}
				if (data.totalSteps != null) {
					config.set('totalSteps', data.totalSteps);
				} else {
					config.delete('totalSteps');
				}
				break;
			case 'failure':
				if (data.failureMessage != null && data.failureMessage !== '') {
					config.set('failureMessage', data.failureMessage);
				} else {
					config.delete('failureMessage');
				}
				if (data.errorResultMapping && data.errorResultMapping.length > 0) {
					config.set('errorResultMapping', data.errorResultMapping);
				} else {
					config.delete('errorResultMapping');
				}
				break;
			case 'trigger':
				config.set('source', data.source);
				config.set('concurrency', data.concurrency);
				config.set('payloadMapping', data.payloadMapping ?? []);
				config.set('enabled', data.enabled ?? false);
				break;
			case 'sub_workflow':
				config.set('templateId', data.templateId);
				config.set('versionPin', data.versionPin ?? { mode: 'latest' });
				if (data.inputMapping && data.inputMapping.length > 0) {
					config.set('inputMapping', data.inputMapping);
				} else {
					config.delete('inputMapping');
				}
				// Persist the child's input-contract snapshot symmetric with `output`
				// below. Without this the SubWorkflowSection contract effect's
				// `portsEqual(data.inputContract, c.input)` never settles true and it
				// re-fetches the io-contract on a loop (the "picker refreshes forever"
				// bug). Delete the key when cleared so it round-trips as unset.
				if (data.inputContract) {
					config.set('inputContract', data.inputContract);
				} else {
					config.delete('inputContract');
				}
				config.set(
					'output',
					data.output ?? { id: 'out', label: 'Result', fields: [] }
				);
				break;
			case 'agent':
				config.set('model', data.model);
				config.set('userPrompt', data.userPrompt);
				if (data.systemPrompt) {
					config.set('systemPrompt', data.systemPrompt);
				} else {
					config.delete('systemPrompt');
				}
				if (data.responseFormat) {
					config.set('responseFormat', data.responseFormat);
				} else {
					config.delete('responseFormat');
				}
				config.set('maxTurns', data.maxTurns ?? 1);
				if (data.stopWhen) {
					config.set('stopWhen', data.stopWhen);
				} else {
					config.delete('stopWhen');
				}
				config.set('contextStrategy', data.contextStrategy ?? 'none');
				config.set('onToolError', data.onToolError ?? 'feedback');
				// Staged-asset bindings (docs/20 §5). Symmetric with automated_step:
				// only touch the Y.Map key when the field is explicitly present in
				// the incoming data. See the automated_step case comment for the
				// full rationale.
				if ('assetBindings' in data) {
					if (data.assetBindings && data.assetBindings.length > 0) {
						config.set('assetBindings', data.assetBindings);
					} else {
						config.delete('assetBindings');
					}
				}
				break;
			case 'delay':
				config.set('durationMsExpr', data.durationMsExpr ?? '5000');
				break;
			case 'timeout':
				config.set('durationMsExpr', data.durationMsExpr ?? '60000');
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
