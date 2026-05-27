// HPI domain types — re-exported from the main app
// These are the canonical type definitions for HPI tasks, fields, blocks, and processes.

export const TASK_FIELD_KINDS = [
	'text',
	'textarea',
	'number',
	'select',
	'checkbox',
	'file',
	'signature',
	'radio',
	'date',
	'range',
	'rating'
] as const;

export type TaskFieldKind = (typeof TASK_FIELD_KINDS)[number];

export type SignatureMode = 'draw';

export type SignatureValue = {
	mode: SignatureMode;
	data: string;
	timestamp: string;
};

export type SignatureAudit = {
	client_ip: string;
	user_agent: string;
	payload_sha256: string;
	timestamp: string;
	user_id: string;
	user_email: string;
	user_name: string;
};

export type SelectOption = {
	value: string;
	label: string;
};

export type TaskField = {
	name: string;
	label: string;
	kind: TaskFieldKind;
	required?: boolean;
	placeholder?: string;
	description_mdsvex?: string;
	options?: SelectOption[];
	accept?: string;
	max_file_size?: number;
	max_files?: number;
	signature_mode?: SignatureMode;
	pen_color?: string;
	min?: number;
	max?: number;
	step?: number;
	max_rating?: number;
	include_time?: boolean;
};

export type DownloadItem = {
	url: string;
	filename: string;
	size?: number;
	mime_type?: string;
	thumbnail_url?: string;
	description?: string;
};

export const CHART_TYPES = ['area', 'bar', 'line', 'pie'] as const;
export type ChartType = (typeof CHART_TYPES)[number];

export type ChartSeries = {
	key: string;
	label?: string;
	color?: string;
};

export type ChartBlockData = {
	chart_type: ChartType;
	data: Record<string, unknown>[];
	x?: string;
	series?: ChartSeries[];
	caption?: string;
	height?: string;
};

export type TaskBlock =
	| { type: 'input'; field: TaskField }
	| { type: 'mdsvex'; content: string }
	| { type: 'download'; downloads: DownloadItem[] }
	| { type: 'table'; headers: string[]; rows: string[][]; alignments?: ('left' | 'center' | 'right')[]; caption?: string }
	| { type: 'image'; url: string; alt?: string; caption?: string }
	| { type: 'callout'; severity: 'info' | 'warning' | 'error' | 'success'; title?: string; content: string }
	| { type: 'pdf'; url: string; filename?: string; caption?: string; height?: string }
	| { type: 'chart'; chart_type: ChartType; data: Record<string, unknown>[]; x?: string; series?: ChartSeries[]; caption?: string; height?: string; x_label?: string; y_label?: string }
	| { type: 'divider' }
	/**
	 * Feature B — render N copies of a sub-form, one per element of an
	 * upstream array. `items_ref` carries exactly one `[*]` iteration
	 * boundary (e.g. `extract.tasks[*]`); the renderer reads
	 * `taskData[<pre-[*]-path>]` for the resolved array and instantiates
	 * `fields` once per element. The submitted form value is collected
	 * under `output_slug` as a `{ name: value }[]` array.
	 */
	| {
		type: 'repeater';
		items_ref: string;
		item_label_ref?: string;
		fields: TaskField[];
		output_slug: string;
	};

export type TaskStep = {
	id: string;
	title: string;
	description_mdsvex?: string;
	blocks: TaskBlock[];
};

export type TaskSinkEvent = 'completed' | 'cancelled' | 'failed';

export type TaskSink =
	| { type: 'nats'; subject: string; events?: TaskSinkEvent[] }
	| { type: 'webhook'; url: string; secret?: string; events?: TaskSinkEvent[] }
	| { type: 'email'; to: string; events?: TaskSinkEvent[] };

export type HumanTask = {
	task_id: string;
	org_id: string;
	correlation_key?: string;
	title: string;
	instructions_mdsvex?: string;
	steps: TaskStep[];
	status: 'pending' | 'completed' | 'cancelled' | 'failed';
	created_at: string;
	completed_at?: string;
	completed_by?: string;
	data?: Record<string, unknown>;
	/**
	 * Feature B — resolved upstream data the compiler stages into the
	 * `HumanTaskRequest` envelope for `TaskForm` to consume. Carries
	 * `<head>.<...pre>` paths for every Repeater block's `items_ref` so
	 * the renderer can resolve `getAtPath(payload, [head, ...pre])` to
	 * the array it iterates. Empty / undefined when no Repeater is in
	 * play.
	 */
	payload?: Record<string, unknown>;
	cancelled_at?: string;
	cancel_reason?: string;
	failed_at?: string;
	fail_reason?: string;
	sinks?: TaskSink[];
	process_id?: string;
	process_step?: string;
	assignee_id?: string;
	assignee_type?: 'member' | 'external';
	created_by?: string;
	template_id?: string;
	metadata?: Record<string, string>;
	duration_ms?: number;
};

export type ProcessStepDef = {
	key: string;
	label: string;
	human?: boolean;
};

export type ProcessTimelineEntry = {
	step: string;
	label: string;
	status: 'pending' | 'running' | 'completed' | 'failed';
	human: boolean;
	task_id?: string;
	started_at?: string;
	completed_at?: string;
	detail?: string;
	progress_message?: string;
	progress_percent?: number;
	execution_id?: string;
	duration_ms?: number;
};

export type ProcessState = {
	process_id: string;
	namespace: string;
	name: string;
	description?: string;
	step_defs: ProcessStepDef[];
	started_at: string;
	status: 'running' | 'completed' | 'failed';
	current_step?: string;
	timeline: ProcessTimelineEntry[];
	task_ids: string[];
	completed_at?: string;
	error?: string;
	template_id?: string;
	metadata?: Record<string, string>;
	duration_ms?: number;
};
