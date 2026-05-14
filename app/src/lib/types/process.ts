/**
 * Frontend-only process shapes. The wire types (`HpiProcess`, `HpiTask`,
 * `HpiMetric`, `HpiMetricSummary`, `HpiLog`, `ProcessDetail`, `ProcessStats`)
 * are exported from `$lib/api/client` — import them from there.
 *
 * What lives here:
 *   - Pagination envelope for the process/task/log/artifact list endpoints,
 *     which use a custom DSL not modeled in the spec yet.
 *   - Step-timeline shapes derived client-side from a process's history.
 */
export interface PaginatedProcessResponse<T> {
	items: T[];
	total: number;
	page: number;
	page_size: number;
	total_pages: number;
	has_next: boolean;
	has_previous: boolean;
}

export interface StepDefinition {
	key: string;
	label: string;
	human: boolean;
}

export interface StepEvent {
	started: string | null;
	completed: string | null;
	timestamp: string;
}

export interface ProcessTimelineEntry {
	step: string;
	label: string;
	status: 'pending' | 'running' | 'completed' | 'failed';
	human: boolean;
	started_at?: string;
	completed_at?: string;
	duration_ms?: number;
	iterations?: number;
	completed_iterations?: number;
}
