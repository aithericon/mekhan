/**
 * Frontend-only process shapes. The wire types (`HpiProcess`, `HpiTask`,
 * `HpiMetric`, `HpiMetricSummary`, `HpiLog`, `ProcessDetail`, `ProcessStats`)
 * are exported from `$lib/api/client` — import them from there.
 *
 * What lives here:
 *   - Pagination envelope for the process/task/log/artifact list endpoints,
 *     which use a custom DSL not modeled in the spec yet.
 *   - The canonical phase/progress timeline shapes.
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

export interface ProcessTimelineEntry {
	step: string;
	label: string;
	status: 'pending' | 'running' | 'completed' | 'failed' | 'skipped';
	human: boolean;
	started_at?: string;
	completed_at?: string;
	duration_ms?: number;
	iterations?: number;
	completed_iterations?: number;
}

/**
 * Canonical phase/progress model — mirrors
 * `aithericon_executor_domain::{Phase, Progress}`, stored verbatim by the
 * mekhan ingest projector at `process.config.progress`.
 */
export type PhaseStatus = 'pending' | 'running' | 'completed' | 'failed' | 'skipped';

export interface Phase {
	name: string;
	status: PhaseStatus;
	message?: string | null;
	started_at?: string | null;
	ended_at?: string | null;
}

export interface Progress {
	fraction: number;
	message?: string | null;
	current_step: number;
	total_steps: number;
	phases: Phase[];
	updated_at: string;
}
