// Re-export shared types from hpi-ui
export type {
	HumanTask,
	TaskStep,
	TaskBlock,
	TaskField,
	TaskFieldKind,
	ProcessState as HpiProcessState,
	ProcessStepDef,
	ProcessTimelineEntry
} from '@aithericon/hpi-ui/types';

import type { HumanTask, ProcessStepDef } from '@aithericon/hpi-ui/types';

export type TaskStatus = 'pending' | 'completed' | 'cancelled' | 'failed';

/** Response from GET /api/tasks (proxied from HPI) */
export type TaskListResponse = {
	tasks: HumanTask[];
	total: number;
};

/** Process state projected by mekhan-service from NATS */
export type MekhanProcessState = {
	process_id: string;
	namespace: string;
	name: string;
	description?: string;
	step_defs: ProcessStepDef[];
	status: 'running' | 'completed' | 'failed';
	current_step?: string;
	timeline: MekhanTimelineEntry[];
	started_at: string;
	completed_at?: string;
	error?: string;
};

export type MekhanTimelineEntry = {
	step: string;
	label: string;
	status: 'pending' | 'running' | 'completed' | 'failed';
	human: boolean;
	started_at?: string;
	completed_at?: string;
	detail?: string;
	progress_message?: string;
	progress_percent?: number;
	duration_ms?: number;
};
