/**
 * Editor-side helpers around the schema-generated workflow graph types.
 *
 * Wire types (`WorkflowGraph`, `WorkflowNodeData`, `TaskBlockConfig`, ...) come
 * from `$lib/api/client`, which re-exports `components['schemas']` from the
 * generated `schema.d.ts`. This file keeps only frontend-only concerns:
 *   - per-variant aliases (`StartNodeData`, `HumanTaskNodeData`, ...) derived
 *     from the schema's discriminated union, so editor components stay terse
 *   - the visual-editor palette + default-node factory
 *
 * If you find yourself adding a wire-shape type here, that's a signal the
 * Rust DTO is missing something. Add it to `service/src/models/template.rs`
 * and regenerate the spec instead of carrying a frontend-only override.
 */
import type {
	WorkflowNodeData as SchemaWorkflowNodeData,
	TaskBlockConfig as SchemaTaskBlockConfig
} from '$lib/api/client';

export type {
	WorkflowEdge,
	WorkflowGraph,
	WorkflowNodeData,
	TaskStepConfig,
	TaskBlockConfig,
	TaskFieldConfig,
	BranchCondition,
	ExecutionSpecConfig,
	// Schema-emitted enums — re-exported so editor components import a single
	// place. Wire values are snake_case (see Rust models in template.rs).
	CalloutSeverity,
	ImageDisplay,
	TaskFieldKind,
	ExecutionBackendType
} from '$lib/api/client';

export type WorkflowNodeType = SchemaWorkflowNodeData['type'];
/**
 * Canonical discriminant union of every workflow node kind, derived from the
 * generated OpenAPI schema. Use this for exhaustive `Record<NodeKind, …>`
 * registries (node renderers, property-panel sections) so adding/removing a
 * kind in the Rust DTO surfaces as a compile error on the frontend.
 */
export type NodeKind = WorkflowNodeType;
export type WorkflowEdgeType = 'sequence' | 'conditional' | 'loop_back';

// Per-variant aliases extracted from the schema's discriminated union.
export type StartNodeData = Extract<SchemaWorkflowNodeData, { type: 'start' }>;
export type EndNodeData = Extract<SchemaWorkflowNodeData, { type: 'end' }>;
export type HumanTaskNodeData = Extract<SchemaWorkflowNodeData, { type: 'human_task' }>;
export type AutomatedStepNodeData = Extract<SchemaWorkflowNodeData, { type: 'automated_step' }>;
export type DecisionNodeData = Extract<SchemaWorkflowNodeData, { type: 'decision' }>;
export type ParallelSplitNodeData = Extract<SchemaWorkflowNodeData, { type: 'parallel_split' }>;
export type JoinNodeData = Extract<SchemaWorkflowNodeData, { type: 'join' }>;
export type LoopNodeData = Extract<SchemaWorkflowNodeData, { type: 'loop' }>;
export type MapNodeData = Extract<SchemaWorkflowNodeData, { type: 'map' }>;
export type ScopeNodeData = Extract<SchemaWorkflowNodeData, { type: 'scope' }>;
export type LeaseScopeNodeData = Extract<SchemaWorkflowNodeData, { type: 'lease_scope' }>;
export type PhaseUpdateNodeData = Extract<SchemaWorkflowNodeData, { type: 'phase_update' }>;
export type ProgressUpdateNodeData = Extract<SchemaWorkflowNodeData, { type: 'progress_update' }>;
export type FailureNodeData = Extract<SchemaWorkflowNodeData, { type: 'failure' }>;
export type DelayNodeData = Extract<SchemaWorkflowNodeData, { type: 'delay' }>;
export type TimeoutNodeData = Extract<SchemaWorkflowNodeData, { type: 'timeout' }>;
export type TriggerNodeData = Extract<SchemaWorkflowNodeData, { type: 'trigger' }>;
export type SubWorkflowNodeData = Extract<SchemaWorkflowNodeData, { type: 'sub_workflow' }>;
export type AgentNodeData = Extract<SchemaWorkflowNodeData, { type: 'agent' }>;
export type StreamConsumerNodeData = Extract<SchemaWorkflowNodeData, { type: 'stream_consumer' }>;

// Convenience aliases for TaskBlockConfig variants used in editor pickers.
export type InputBlock = Extract<SchemaTaskBlockConfig, { type: 'input' }>;
export type MdsvexBlock = Extract<SchemaTaskBlockConfig, { type: 'mdsvex' }>;
export type CalloutBlock = Extract<SchemaTaskBlockConfig, { type: 'callout' }>;
export type DividerBlock = Extract<SchemaTaskBlockConfig, { type: 'divider' }>;
export type ImageBlock = Extract<SchemaTaskBlockConfig, { type: 'image' }>;
export type FileBlock = Extract<SchemaTaskBlockConfig, { type: 'file' }>;
export type PdfBlock = Extract<SchemaTaskBlockConfig, { type: 'pdf' }>;
export type DownloadBlock = Extract<SchemaTaskBlockConfig, { type: 'download' }>;
export type RepeaterBlockConfig = Extract<SchemaTaskBlockConfig, { type: 'repeater' }>;

/**
 * Create default node data for a given type.
 *
 * Kept frontend-side (not derived from `/api/v1/node-types`) because it
 * constructs typed `WorkflowNodeData` objects with nested defaults — port
 * shapes, default retry policy, default model ref, default trigger source.
 * The palette fires this on every drag-drop, so a round-trip would visibly
 * lag. The exhaustive `switch` is type-checked against the schema-derived
 * `WorkflowNodeType` so a new variant is a compile error here too. Palette
 * metadata (labels, descriptions, kind, protocol flags) flows from the
 * registry via `node-registry.svelte.ts` + `node-palette-meta.ts`.
 */
export function createDefaultNodeData(type: WorkflowNodeType): SchemaWorkflowNodeData {
	switch (type) {
		case 'start':
			return {
				type: 'start',
				label: 'Start',
				initial: { id: 'in', label: 'Input', fields: [] }
			};
		case 'end':
			return { type: 'end', label: 'End' };
		case 'human_task':
			return {
				type: 'human_task',
				label: 'Human Task',
				taskTitle: 'New Task',
				steps: [
					{
						id: crypto.randomUUID(),
						title: 'Step 1',
						blocks: []
					}
				]
			};
		case 'automated_step':
			return {
				type: 'automated_step',
				label: 'Automated Step',
				executionSpec: {
					backendType: 'python',
					entrypoint: 'main.py',
					config: {}
				},
				retryPolicy: { maxRetries: 3, backoff: 'immediate', baseDelayMs: 0 },
				deploymentModel: { mode: 'executor' }
			};
		case 'decision':
			return {
				type: 'decision',
				label: 'Decision',
				conditions: []
			};
		case 'parallel_split':
			return { type: 'parallel_split', label: 'Parallel Split' };
		case 'join':
			return {
				type: 'join',
				label: 'Join',
				mode: 'all',
				mergeStrategy: 'shallow_last_wins',
				output: { id: 'out', label: 'Output', fields: [] }
			};
		case 'loop':
			return {
				type: 'loop',
				label: 'Loop',
				maxIterations: 3,
				loopCondition: 'true',
				accumulators: []
			};
		case 'map':
			return {
				type: 'map',
				label: 'Map',
				itemsRef: '',
				itemVar: 'item',
				resultVar: '',
				output: { id: 'out', label: 'Element', fields: [] }
			};
		case 'scope':
			return { type: 'scope', label: 'Scope' };
		case 'lease_scope':
			return { type: 'lease_scope', label: 'Lease Scope', lease: { scheduler: '' } };
		case 'phase_update':
			return {
				type: 'phase_update',
				label: 'Phase Update',
				phaseName: 'New phase',
				status: 'running'
			};
		case 'progress_update':
			return {
				type: 'progress_update',
				label: 'Progress Update',
				fraction: 0
			};
		case 'failure':
			return {
				type: 'failure',
				label: 'Failure'
			};
		case 'delay':
			return {
				type: 'delay',
				label: 'Delay',
				durationMsExpr: '5000'
			};
		case 'timeout':
			return {
				type: 'timeout',
				label: 'Timeout',
				durationMsExpr: '60000'
			};
		case 'trigger':
			return {
				type: 'trigger',
				label: 'Trigger',
				source: { kind: 'manual', form: [] },
				concurrency: 'allow',
				payloadMapping: [],
				enabled: true
			};
		case 'sub_workflow':
			return {
				type: 'sub_workflow',
				label: 'Sub-workflow',
				templateId: '',
				versionPin: { mode: 'latest' },
				inputMapping: [],
				output: { id: 'out', label: 'Result', fields: [] }
			};
		case 'agent':
			return {
				type: 'agent',
				label: 'Agent',
				model: { provider: 'anthropic', model: 'claude-haiku-4-5-20251001' },
				userPrompt: '',
				maxTurns: 1,
				contextStrategy: 'none',
				onToolError: 'feedback',
				retryPolicy: { maxRetries: 3, backoff: 'immediate', baseDelayMs: 0 },
				deploymentModel: { mode: 'executor' }
			};
		case 'stream_consumer':
			return {
				type: 'stream_consumer',
				label: 'Stream Consumer',
				resultVar: 'item',
				reduce: { kind: 'array' }
			};
	}
}
