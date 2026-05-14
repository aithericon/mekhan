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
export type ParallelJoinNodeData = Extract<SchemaWorkflowNodeData, { type: 'parallel_join' }>;
export type LoopNodeData = Extract<SchemaWorkflowNodeData, { type: 'loop' }>;
export type ScopeNodeData = Extract<SchemaWorkflowNodeData, { type: 'scope' }>;

// Convenience aliases for TaskBlockConfig variants used in editor pickers.
export type InputBlock = Extract<SchemaTaskBlockConfig, { type: 'input' }>;
export type MdsvexBlock = Extract<SchemaTaskBlockConfig, { type: 'mdsvex' }>;
export type CalloutBlock = Extract<SchemaTaskBlockConfig, { type: 'callout' }>;
export type DividerBlock = Extract<SchemaTaskBlockConfig, { type: 'divider' }>;
export type ImageBlock = Extract<SchemaTaskBlockConfig, { type: 'image' }>;
export type FileBlock = Extract<SchemaTaskBlockConfig, { type: 'file' }>;
export type PdfBlock = Extract<SchemaTaskBlockConfig, { type: 'pdf' }>;

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
		type: 'parallel_join',
		label: 'Parallel Join',
		description: 'Wait for all parallel paths',
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
				}
			};
		case 'decision':
			return {
				type: 'decision',
				label: 'Decision',
				conditions: []
			};
		case 'parallel_split':
			return { type: 'parallel_split', label: 'Parallel Split' };
		case 'parallel_join':
			return { type: 'parallel_join', label: 'Parallel Join' };
		case 'loop':
			return {
				type: 'loop',
				label: 'Loop',
				maxIterations: 3,
				loopCondition: 'true'
			};
		case 'scope':
			return { type: 'scope', label: 'Scope' };
	}
}
