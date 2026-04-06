export interface HpiProcess {
	process_id: string;
	name: string | null;
	kind: string | null;
	status: string;
	owner: string | null;
	config: Record<string, unknown>;
	created_at: string;
	updated_at: string;
}

export interface HpiTask {
	id: string;
	process_id: string;
	title: string;
	status: string;
	assignee: string | null;
	detail: Record<string, unknown>;
	created_at: string;
	completed_at: string | null;
}

export interface HpiMetric {
	process_id: string;
	key: string;
	value: number;
	timestamp: string;
}

export interface HpiMetricSummary {
	key: string;
	count: number;
	min_value: number;
	max_value: number;
	avg_value: number;
	last_value: number;
	last_timestamp: string;
}

export interface HpiLog {
	id: number;
	process_id: string;
	level: string;
	source: string | null;
	message: string;
	detail: Record<string, unknown>;
	timestamp: string;
}

export interface ProcessDetail extends HpiProcess {
	tasks: HpiTask[];
	recent_metrics: HpiMetric[];
	recent_logs: HpiLog[];
	artifact_count: number;
}

export interface ProcessStats {
	total: number;
	active: number;
	completed: number;
	failed: number;
}

export interface PaginatedProcessResponse<T> {
	items: T[];
	total: number;
	page: number;
	page_size: number;
	total_pages: number;
	has_next: boolean;
	has_previous: boolean;
}
