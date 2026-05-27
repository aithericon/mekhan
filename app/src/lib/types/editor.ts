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
export type ScopeNodeData = Extract<SchemaWorkflowNodeData, { type: 'scope' }>;
export type PhaseUpdateNodeData = Extract<SchemaWorkflowNodeData, { type: 'phase_update' }>;
export type ProgressUpdateNodeData = Extract<SchemaWorkflowNodeData, { type: 'progress_update' }>;
export type FailureNodeData = Extract<SchemaWorkflowNodeData, { type: 'failure' }>;
export type DelayNodeData = Extract<SchemaWorkflowNodeData, { type: 'delay' }>;
export type TimeoutNodeData = Extract<SchemaWorkflowNodeData, { type: 'timeout' }>;
export type TriggerNodeData = Extract<SchemaWorkflowNodeData, { type: 'trigger' }>;
export type SubWorkflowNodeData = Extract<SchemaWorkflowNodeData, { type: 'sub_workflow' }>;
export type AgentNodeData = Extract<SchemaWorkflowNodeData, { type: 'agent' }>;

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

/** Node type metadata for the sidebar palette. */
export type NodePaletteItem = {
	type: WorkflowNodeType;
	label: string;
	description: string;
	icon: string;
	color: string;
	maxInstances?: number; // e.g., Start = 1
};

export const NODE_PALETTE: NodePaletteItem[] = [
	{
		type: 'start',
		label: 'Start',
		description: 'Entry point of the workflow',
		icon: 'play',
		color: '#22c55e',
		maxInstances: 1
	},
	{
		type: 'end',
		label: 'End',
		description: 'Terminal state of the workflow',
		icon: 'square',
		color: '#ef4444'
	},
	{
		type: 'human_task',
		label: 'Human Task',
		description: 'Form-based task for human operators',
		icon: 'user',
		color: '#3b82f6'
	},
	{
		type: 'automated_step',
		label: 'Automated Step',
		description: 'Automated execution (Python, Docker, etc.)',
		icon: 'cpu',
		color: '#8b5cf6'
	},
	{
		type: 'decision',
		label: 'Decision',
		description: 'Conditional branching based on data',
		icon: 'git-branch',
		color: '#f59e0b'
	},
	{
		type: 'parallel_split',
		label: 'Parallel Split',
		description: 'Fan out to concurrent paths',
		icon: 'git-fork',
		color: '#06b6d4'
	},
	{
		type: 'join',
		label: 'Join',
		description: 'Converge branches — wait for all, or fire on any (XOR-join)',
		icon: 'git-merge',
		color: '#06b6d4'
	},
	{
		type: 'loop',
		label: 'Loop',
		description: 'Retry or iterate with conditions',
		icon: 'repeat',
		color: '#ec4899'
	},
	{
		type: 'scope',
		label: 'Scope',
		description: 'Visual container for grouping nodes',
		icon: 'group',
		color: '#64748b'
	},
	{
		type: 'phase_update',
		label: 'Phase Update',
		description: 'Mark a named phase on the process (within a named process)',
		icon: 'flag',
		color: '#0ea5a4'
	},
	{
		type: 'progress_update',
		label: 'Progress Update',
		description: 'Set process progress fraction (within a named process)',
		icon: 'gauge',
		color: '#c026d3'
	},
	{
		type: 'failure',
		label: 'Failure',
		description: 'Mark the process failed with a message (net continues)',
		icon: 'octagon-x',
		color: '#dc2626'
	},
	{
		type: 'delay',
		label: 'Delay',
		description: 'Pause for a Rhai-evaluated number of ms, then forward the token',
		icon: 'timer',
		color: '#0891b2'
	},
	{
		type: 'timeout',
		label: 'Timeout',
		description: 'Race a body branch against a deadline; cancel body on timer-win',
		icon: 'timer-off',
		color: '#0e7490'
	},
	{
		type: 'trigger',
		label: 'Trigger',
		description: 'Fires the workflow on cron, catalog, webhook, etc.',
		icon: 'zap',
		color: '#fbbf24'
	},
	{
		type: 'sub_workflow',
		label: 'Sub-workflow',
		description: 'Call another template and return its typed result',
		icon: 'workflow',
		color: '#14b8a6'
	},
	{
		type: 'agent',
		label: 'Agent',
		description: 'LLM that calls tagged child tools and loops until a stop condition',
		icon: 'bot',
		color: '#f97316'
	}
];

/** Create default node data for a given type. */
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
				deploymentModel: { mode: 'inline' }
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
				loopCondition: 'true'
			};
		case 'scope':
			return { type: 'scope', label: 'Scope' };
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
				onToolError: 'feedback'
			};
	}
}
