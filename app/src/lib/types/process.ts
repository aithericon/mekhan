export interface HpiProcess {
	trace_id: string;
	name: string | null;
	kind: string | null;
	status: string;
	owner: string | null;
	hpi_process_id: string | null;
	config: Record<string, unknown>;
	created_at: string;
	updated_at: string;
}

export interface HpiTask {
	id: string;
	trace_id: string;
	span_id: string | null;
	title: string;
	status: string;
	assignee: string | null;
	detail: Record<string, unknown>;
	created_at: string;
	completed_at: string | null;
}

export interface HpiMetric {
	trace_id: string;
	span_id: string | null;
	key: string;
	value: number;
	timestamp: string;
}

export interface HpiLog {
	id: number;
	trace_id: string;
	span_id: string | null;
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
