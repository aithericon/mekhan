// Re-export shared types from hpi-ui
export type {
	HumanTask,
	TaskStep,
	TaskBlock,
	TaskField,
	TaskFieldKind,
	ProcessState,
	ProcessStepDef,
	ProcessTimelineEntry
} from '@aithericon/hpi-ui/types';

import type { HumanTask } from '@aithericon/hpi-ui/types';

export type TaskStatus = 'pending' | 'completed' | 'cancelled' | 'failed';

/** Response from GET /api/tasks (proxied from HPI) */
export type TaskListResponse = {
	tasks: HumanTask[];
	total: number;
};
