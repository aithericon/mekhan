/** Base properties shared by all nodes */
export type BaseNodeData = {
	label: string;
	description?: string;
};

/** Start block - entry point of the workflow */
export type StartNodeData = BaseNodeData & {
	type: 'start';
	initialData?: Record<string, unknown>;
};

/** End block - terminal state */
export type EndNodeData = BaseNodeData & {
	type: 'end';
};

/** Human Task block - creates a human-ui task */
export type HumanTaskNodeData = BaseNodeData & {
	type: 'human_task';
	taskTitle: string;
	instructionsMdsvex?: string;
	steps: TaskStepConfig[];
};

/** Automated Step block - triggers executor */
export type AutomatedStepNodeData = BaseNodeData & {
	type: 'automated_step';
	executionSpec: ExecutionSpecConfig;
};

/** Decision/Branch block - conditional routing */
export type DecisionNodeData = BaseNodeData & {
	type: 'decision';
	conditions: BranchCondition[];
	defaultBranch?: string;
};

/** Parallel Split block - fan out to concurrent paths */
export type ParallelSplitNodeData = BaseNodeData & {
	type: 'parallel_split';
};

/** Parallel Join block - synchronization point */
export type ParallelJoinNodeData = BaseNodeData & {
	type: 'parallel_join';
};

/** Loop block - retry or iterate */
export type LoopNodeData = BaseNodeData & {
	type: 'loop';
	maxIterations: number;
	loopCondition: string;
};

export type WorkflowNodeData =
	| StartNodeData
	| EndNodeData
	| HumanTaskNodeData
	| AutomatedStepNodeData
	| DecisionNodeData
	| ParallelSplitNodeData
	| ParallelJoinNodeData
	| LoopNodeData;

export type WorkflowNodeType = WorkflowNodeData['type'];

/** TaskStep configuration (maps to human-ui TaskStep) */
export type TaskStepConfig = {
	id: string;
	title: string;
	descriptionMdsvex?: string;
	blocks: TaskBlockConfig[];
};

/** Block configuration within a task step */
export type TaskBlockConfig =
	| { type: 'input'; field: TaskFieldConfig }
	| { type: 'mdsvex'; content: string }
	| {
			type: 'callout';
			severity: 'info' | 'warning' | 'error' | 'success';
			title?: string;
			content: string;
	  }
	| { type: 'divider' };

export type TaskFieldConfig = {
	name: string;
	label: string;
	kind: 'text' | 'textarea' | 'number' | 'select' | 'checkbox' | 'file' | 'signature';
	required?: boolean;
	placeholder?: string;
	options?: string[];
};

export type BranchCondition = {
	edgeId: string;
	label: string;
	guard: string;
};

export type ExecutionSpecConfig = {
	backendType: 'python' | 'process' | 'docker';
	config: Record<string, unknown>;
};

/** Edge types in the workflow editor */
export type WorkflowEdgeType = 'sequence' | 'conditional' | 'loop_back';

export type WorkflowEdge = {
	id: string;
	source: string;
	target: string;
	sourceHandle?: string;
	label?: string;
	type: WorkflowEdgeType;
};

/** The full workflow graph as stored in the database */
export type WorkflowGraph = {
	nodes: Array<{
		id: string;
		type: WorkflowNodeType;
		position: { x: number; y: number };
		data: WorkflowNodeData;
	}>;
	edges: WorkflowEdge[];
	viewport?: { x: number; y: number; zoom: number };
};

/** Node type metadata for the sidebar palette */
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
	}
];

/** Create default node data for a given type */
export function createDefaultNodeData(type: WorkflowNodeType): WorkflowNodeData {
	switch (type) {
		case 'start':
			return { type: 'start', label: 'Start' };
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
	}
}
