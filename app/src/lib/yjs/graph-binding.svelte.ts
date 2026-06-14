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
	AgentNodeData,
	StreamSourceNodeData,
	StreamSinkNodeData
} from '$lib/types/editor';
import { mintNodeId, mintEdgeId } from '$lib/editor/ids';
import { sanitizeSlug } from '$lib/editor/sanitize-slug';

/**
 * One node snapshotted out of the Y.Doc for copy/paste: config flattened to
 * plain JSON, files flattened to plain strings — fully detached from the
 * source doc, so the clipboard survives the source being edited or deleted.
 */
export interface ClipboardNode {
	id: string;
	type: WorkflowNodeType;
	position: { x: number; y: number };
	label: string;
	description?: string;
	parentId?: string;
	width?: number;
	height?: number;
	/** Explicit slug at copy time — used ONLY to remap `<slug>.<field>`
	 *  references on paste; never re-set on the clone (see copySubgraph). */
	slug?: string;
	config: Record<string, unknown>;
	files: Record<string, string>;
}

export interface GraphClipboard {
	nodes: ClipboardNode[];
	edges: WorkflowEdge[];
}

// Config values are JSON-plain semantically (they round-trip through Yjs
// encoding), but at runtime they're often Svelte `$state` proxies —
// writeDataToConfig stores the editor's reactive objects as-is, and
// structuredClone REJECTS proxies. JSON round-trip both detaches and
// de-proxies in one move.
function jsonClone<T>(value: T): T {
	return value === undefined ? value : (JSON.parse(JSON.stringify(value)) as T);
}

/**
 * Rewrite producer-namespaced `<slug>.<field>` references in a string through
 * an old-slug → new-slug map. Matches a whole identifier immediately followed
 * by a field access (`\b<slug>\b(?=\.)`); slugs are sanitized
 * (`^[a-z][a-z0-9_]*$`), so no regex escaping is needed and `_` being a word
 * char rules out partial-slug hits.
 */
function remapSlugRefs(text: string, slugMap: Map<string, string>): string {
	let out = text;
	for (const [oldSlug, newSlug] of slugMap) {
		out = out.replace(new RegExp(`\\b${oldSlug}\\b(?=\\.)`, 'g'), newSlug);
	}
	return out;
}

/** Deep-walk a JSON-plain config value, remapping refs in every string leaf.
 *  Keys are never refs (mapping keys name the CONSUMER side), so only values
 *  are rewritten. Returns fresh structures — never mutates the input. */
function remapRefsDeep(value: unknown, slugMap: Map<string, string>): unknown {
	if (typeof value === 'string') return remapSlugRefs(value, slugMap);
	if (Array.isArray(value)) return value.map((v) => remapRefsDeep(v, slugMap));
	if (value && typeof value === 'object') {
		return Object.fromEntries(
			Object.entries(value).map(([k, v]) => [k, remapRefsDeep(v, slugMap)])
		);
	}
	return value;
}

/**
 * YjsGraphBinding observes a Y.Doc and exposes a reactive WorkflowGraph.
 *
 * Y.Doc schema:
 *   Y.Map("meta")     ← { name, description, author_id }
 *   Y.Map("nodes")    ← keyed by nodeId → Y.Map { type, label, description, config (Y.Map), position, files (Y.Map → Y.Text), parentId?, width?, height? }
 *   Y.Array("edges")  ← [{ id, source, target, sourceHandle?, label?, type, join? }]
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
	private undoManager: Y.UndoManager | null = null;

	graph: WorkflowGraph = $state({ nodes: [], edges: [] });
	canUndo = $state(false);
	canRedo = $state(false);

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
				type: (item.type as WorkflowEdgeType) ?? 'sequence',
				// Consumer-side channel join discipline (docs/25). Stored only when
				// 'gather' — absent ⇒ the 'each' default — so only materialize the
				// key when it's actually set (legacy edges keep their exact shape).
				...(item.join === 'gather' ? { join: 'gather' as const } : {})
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
			case 'human_task': {
				type HumanTaskDataT = Extract<WorkflowNodeData, { type: 'human_task' }>;
				return {
					...base,
					type: 'human_task',
					taskTitle: (config?.taskTitle as string) ?? '',
					instructionsMdsvex: config?.instructionsMdsvex as string | undefined,
					steps: (config?.steps as WorkflowNodeData extends { steps: infer S } ? S : never) ?? [],
					// Opt-in dynamic-steps source: a `<slug>.<field>` ref to an upstream
					// producer that emits the form blocks at runtime. Undefined ⇒ static
					// `steps` authoring. (Not yet in the generated schema → cast.)
					stepsRef: config?.stepsRef as string | undefined,
					// Capacity binding (docs/33): the consent-acceptance pool this task is
					// offered to + its placement Requirements. Round-trips whole +
					// conditionally so a collaborative edit doesn't drop the offer
					// binding (mirrors `automated_step`'s deploymentModel/requirements).
					...(config?.capacity
						? { capacity: config.capacity as HumanTaskDataT['capacity'] }
						: {}),
					...(config?.requirements
						? { requirements: config.requirements as HumanTaskDataT['requirements'] }
						: {})
				} as WorkflowNodeData;
			}
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
					// Frozen library-node branding snapshot (decision 12) — round-trips
					// like inputContract so the canvas renders the branded card.
					...(config?.sourceCoordinate
						? {
								sourceCoordinate:
									config.sourceCoordinate as SubWorkflowNodeData['sourceCoordinate']
							}
						: {}),
					...(config?.presentation
						? {
								presentation:
									config.presentation as SubWorkflowNodeData['presentation']
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
			case 'stream_source': {
				// `channels` (docs/25) is the node's ONLY config — the whole array
				// round-trips as one value, mirroring automated_step. It MUST be
				// read back here or the editor reconstruction drops the channels
				// and the node loses its (only) handles.
				const channels = config?.channels as StreamSourceNodeData['channels'] | undefined;
				return {
					...base,
					type: 'stream_source',
					...(channels && channels.length > 0 ? { channels } : {})
				};
			}
			case 'stream_sink': {
				const channels = config?.channels as StreamSinkNodeData['channels'] | undefined;
				return {
					...base,
					type: 'stream_sink',
					...(channels && channels.length > 0 ? { channels } : {})
				};
			}
		}
	}

	// --- Undo / redo ---

	/**
	 * Local-only undo over the graph's shared types (nodes + edges). Tracked
	 * origins default to `{null}` — exactly the binding's own `doc.transact()`
	 * mutations. Remote updates apply with the WS provider as origin
	 * (ws-provider.ts `Y.applyUpdate(doc, payload, this)`), so collaborators'
	 * edits and the initial server sync never enter this stack: undo only ever
	 * reverts what THIS client did. `meta` and `viewport` are deliberately out
	 * of scope (rename goes through the REST API; pan/zoom isn't an edit).
	 */
	enableUndo(): void {
		if (this.undoManager) return;
		this.undoManager = new Y.UndoManager([this.yNodes, this.yEdges]);
		const sync = () => {
			this.canUndo = this.undoManager?.canUndo() ?? false;
			this.canRedo = this.undoManager?.canRedo() ?? false;
		};
		this.undoManager.on('stack-item-added', sync);
		this.undoManager.on('stack-item-popped', sync);
		this.undoManager.on('stack-cleared', sync);
		this.undoManager.on('stack-item-updated', sync);
	}

	undo(): void {
		this.undoManager?.undo();
	}

	redo(): void {
		this.undoManager?.redo();
	}

	/**
	 * Drop all undo history. Called after programmatic load-time writes (the
	 * SubWorkflow contract backfill) so the user's first Cmd+Z reverts THEIR
	 * first edit, not an invisible bookkeeping patch.
	 */
	clearUndoHistory(): void {
		this.undoManager?.clear();
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
			// Join discipline: normalize-to-default — only the non-default
			// 'gather' is written ('each'/null/undefined stay unset) so legacy
			// graphs keep a stable byte shape.
			if (edge.join === 'gather') obj.join = 'gather';
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

	/**
	 * Set or clear an edge's consumer-side channel join discipline (docs/25).
	 * `'gather'` writes the key; `null` or `'each'` DELETES it — 'each' is the
	 * implicit default, so it round-trips as "unset" and legacy graphs stay
	 * byte-stable. Y.Array entries are plain immutable objects, so an update is
	 * the same delete+insert-at-index dance removeEdge/addEdge use, wrapped in
	 * one transaction so coauthors never observe the edge missing.
	 */
	updateEdgeJoin(edgeId: string, join: 'gather' | 'each' | null): void {
		this.doc.transact(() => {
			let index = -1;
			this.yEdges.forEach((edge, i) => {
				if (edge.id === edgeId) index = i;
			});
			if (index < 0) return;
			const obj: Record<string, unknown> = { ...this.yEdges.get(index) };
			if (join === 'gather') obj.join = 'gather';
			else delete obj.join;
			this.yEdges.delete(index, 1);
			this.yEdges.insert(index, [obj]);
		});
	}

	// --- Clipboard (copy / paste / duplicate) ---

	/** World position of a node — its stored (parent-relative) position summed
	 *  up the `parentId` chain. Mirrors WorkflowCanvas.worldPosOf but reads the
	 *  Y.Doc directly so clipboard logic stays canvas-free and unit-testable. */
	private worldPositionOf(nodeId: string): { x: number; y: number } {
		let x = 0;
		let y = 0;
		let cur: string | undefined = nodeId;
		const seen = new Set<string>();
		while (cur && !seen.has(cur)) {
			seen.add(cur);
			const yNode = this.yNodes.get(cur);
			if (!yNode || !(yNode instanceof Y.Map)) break;
			const pos =
				(yNode.get('position') as { x: number; y: number } | undefined) ?? { x: 0, y: 0 };
			x += pos.x;
			y += pos.y;
			cur = yNode.get('parentId') as string | undefined;
		}
		return { x, y };
	}

	/**
	 * Snapshot a node set (+ the edges fully inside it) into a plain,
	 * doc-detached clipboard. Pure read — no mutation, no id minting (that's
	 * pasteSubgraph's job). Selection semantics:
	 *  - a selected container brings ALL its descendants (transitively), so a
	 *    copied Scope/Loop pastes with its body intact;
	 *  - a child selected WITHOUT its container is snapshotted as a top-level
	 *    node at its WORLD position (parentId dropped) — the simple semantics;
	 *  - edges with either endpoint outside the (expanded) set are dropped;
	 *  - explicit `slug`s are deliberately NOT re-applied to clones: two nodes
	 *    sharing an explicit slug is CompileError::SlugConflict, so clones fall
	 *    back to the compiler's id-derived default slug instead. The copy-time
	 *    slug still rides the clipboard so pasteSubgraph can remap in-set
	 *    `<slug>.<field>` references onto the clones.
	 */
	copySubgraph(nodeIds: string[]): GraphClipboard {
		const selected = new Set(nodeIds.filter((id) => this.yNodes.has(id)));
		// Expand to descendants of selected containers — fixpoint over parentId
		// so nested containers (loop inside lease_scope) come along too.
		let grew = true;
		while (grew) {
			grew = false;
			this.yNodes.forEach((yNode, id) => {
				if (selected.has(id) || !(yNode instanceof Y.Map)) return;
				const pid = yNode.get('parentId') as string | undefined;
				if (pid && selected.has(pid)) {
					selected.add(id);
					grew = true;
				}
			});
		}

		const nodes: ClipboardNode[] = [];
		for (const id of selected) {
			const yNode = this.yNodes.get(id);
			if (!yNode || !(yNode instanceof Y.Map)) continue;
			const parentId = yNode.get('parentId') as string | undefined;
			const parentInSet = parentId != null && selected.has(parentId);
			const posRaw =
				(yNode.get('position') as { x: number; y: number } | undefined) ?? { x: 0, y: 0 };
			const description = yNode.get('description') as string | undefined;
			const width = yNode.get('width') as number | undefined;
			const height = yNode.get('height') as number | undefined;
			const slug = yNode.get('slug') as string | undefined;
			const configMap = yNode.get('config');
			const filesMap = yNode.get('files');
			const files: Record<string, string> = {};
			if (filesMap instanceof Y.Map) {
				filesMap.forEach((value: unknown, name: string) => {
					if (value instanceof Y.Text) files[name] = value.toString();
				});
			}
			nodes.push({
				id,
				type: yNode.get('type') as WorkflowNodeType,
				// Children keep their parent-relative position; an orphaned child
				// (container not in the set) flattens to its world position.
				position: parentInSet ? { ...posRaw } : this.worldPositionOf(id),
				label: (yNode.get('label') as string) ?? '',
				...(description ? { description } : {}),
				...(parentInSet ? { parentId } : {}),
				...(width != null ? { width } : {}),
				...(height != null ? { height } : {}),
				// Carried for paste-time REF REMAPPING only — pasteSubgraph never
				// sets it on the clone (see the slug note in the doc comment).
				...(slug && slug.trim() !== '' ? { slug } : {}),
				config:
					configMap instanceof Y.Map
						? jsonClone(Object.fromEntries(configMap.entries()))
						: {},
				files
			});
		}

		const edges: WorkflowEdge[] = [];
		this.yEdges.forEach((item) => {
			const source = item.source as string;
			const target = item.target as string;
			if (!selected.has(source) || !selected.has(target)) return;
			edges.push({
				id: item.id as string,
				source,
				target,
				sourceHandle: item.sourceHandle as string | undefined,
				targetHandle: item.targetHandle as string | undefined,
				label: item.label as string | undefined,
				type: (item.type as WorkflowEdgeType) ?? 'sequence',
				...(item.join === 'gather' ? { join: 'gather' as const } : {})
			});
		});

		return { nodes, edges };
	}

	/**
	 * Insert a clipboard into the doc with FRESH ids: every node gets a new
	 * minted id, in-set `parentId`s, edge endpoints AND `<slug>.<field>`
	 * references (in config strings + file text) are remapped through the
	 * old→new map, edge ids are re-minted. The `offset` shifts paste
	 * roots only — children of a pasted container keep their parent-relative
	 * position (they ride the container's shift). ONE doc.transact(), so a
	 * single undo reverts the whole paste. Returns the new node ids.
	 */
	pasteSubgraph(
		clip: GraphClipboard,
		offset: { x: number; y: number } = { x: 24, y: 24 }
	): string[] {
		if (clip.nodes.length === 0) return [];
		const idMap = new Map<string, string>();
		for (const n of clip.nodes) idMap.set(n.id, mintNodeId());

		// Producer-namespaced `<slug>.<field>` refs between copied siblings must
		// follow the clones: after a same-doc paste the ORIGINAL producers still
		// exist, so an un-rewritten ref compiles clean but silently reads from
		// the original across the copy boundary. Map each in-set producer's
		// effective slug at copy time (explicit slug, else the id-derived
		// default) to the clone's default slug — the freshly minted id, which is
		// already sanitize_slug-stable verbatim (see mintNodeId).
		const slugMap = new Map<string, string>();
		for (const n of clip.nodes) {
			const oldSlug =
				n.slug && n.slug.trim() !== '' ? sanitizeSlug(n.slug) : sanitizeSlug(n.id);
			slugMap.set(oldSlug, idMap.get(n.id)!);
		}

		this.doc.transact(() => {
			for (const n of clip.nodes) {
				const newId = idMap.get(n.id)!;
				const newParentId = n.parentId ? idMap.get(n.parentId) : undefined;
				const yNode = new Y.Map<unknown>();
				yNode.set('type', n.type);
				yNode.set(
					'position',
					newParentId
						? { ...n.position }
						: { x: n.position.x + offset.x, y: n.position.y + offset.y }
				);
				yNode.set('label', n.label);
				if (n.description) yNode.set('description', n.description);
				if (newParentId) yNode.set('parentId', newParentId);
				if (n.width != null) yNode.set('width', n.width);
				if (n.height != null) yNode.set('height', n.height);
				// No `slug` — see copySubgraph: clones use the id-derived default.

				const config = new Y.Map<unknown>();
				for (const [key, value] of Object.entries(n.config)) {
					// Remap in-set refs onto the clones; remapRefsDeep returns fresh
					// structures, so repeated pastes of one clipboard never share
					// mutable object state either.
					config.set(key, remapRefsDeep(jsonClone(value), slugMap));
				}
				yNode.set('config', config);

				const files = new Y.Map<Y.Text>();
				for (const [name, text] of Object.entries(n.files)) {
					// Python sources reference upstream producers as `<slug>.<field>`
					// too (the compiler synthesizes read-arcs from them) — same remap.
					files.set(name, new Y.Text(remapSlugRefs(text, slugMap)));
				}
				yNode.set('files', files);

				this.yNodes.set(newId, yNode);
			}

			for (const e of clip.edges) {
				const source = idMap.get(e.source)!;
				const target = idMap.get(e.target)!;
				const obj: Record<string, unknown> = {
					id: mintEdgeId(source, target),
					source,
					target,
					type: e.type
				};
				if (e.sourceHandle) obj.sourceHandle = e.sourceHandle;
				if (e.targetHandle) obj.targetHandle = e.targetHandle;
				if (e.label) obj.label = e.label;
				if (e.join === 'gather') obj.join = 'gather';
				this.yEdges.push([obj]);
			}
		});

		return clip.nodes.map((n) => idMap.get(n.id)!);
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
			case 'human_task': {
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
				// Capacity binding (docs/33) — persist when bound, delete when cleared.
				// Selecting "Anyone" strips both `capacity` and `requirements`; a bare
				// `if (x) set()` would leave a stale key that reappears on reload
				// (same reasoning as automated_step's requirements clear path).
				type HumanTaskDataT = Extract<WorkflowNodeData, { type: 'human_task' }>;
				const cap = (data as HumanTaskDataT).capacity;
				if (cap) config.set('capacity', cap);
				else config.delete('capacity');
				const reqs = (data as HumanTaskDataT).requirements;
				if (reqs) config.set('requirements', reqs);
				else config.delete('requirements');
				break;
			}
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
				// Library-node branding snapshot (decision 12) — persist/clear
				// symmetric with inputContract so it round-trips through the ydoc.
				if (data.sourceCoordinate) {
					config.set('sourceCoordinate', data.sourceCoordinate);
				} else {
					config.delete('sourceCoordinate');
				}
				if (data.presentation) {
					config.set('presentation', data.presentation);
				} else {
					config.delete('presentation');
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
			case 'stream_source':
			case 'stream_sink': {
				// `channels` round-trips whole, conditionally — delete when empty so
				// clearing the last channel removes the stale Yjs key (mirrors the
				// automated_step channels clear path).
				const chans = (data as StreamSourceNodeData | StreamSinkNodeData).channels;
				if (chans && chans.length > 0) config.set('channels', chans);
				else config.delete('channels');
				break;
			}
		}
	}

	destroy(): void {
		this.undoManager?.destroy();
		this.undoManager = null;
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
