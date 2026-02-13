import type { WorkflowGraph } from './editor';

/** Template as returned by the API */
export type Template = {
	id: string;
	name: string;
	description: string;
	base_template_id: string;
	parent_id: string | null;
	version: number;
	is_latest: boolean;
	published: boolean;
	published_at: string | null;
	graph: WorkflowGraph;
	air_json: object | null;
	author_id: string;
	created_at: string;
	updated_at: string;
};

/** Template list item (without full graph) */
export type TemplateSummary = {
	id: string;
	name: string;
	description: string;
	version: number;
	is_latest: boolean;
	published: boolean;
	published_at: string | null;
	created_at: string;
	updated_at: string;
};

/** Create template request */
export type CreateTemplateRequest = {
	name: string;
	description?: string;
	graph?: WorkflowGraph;
	author_id: string;
};

/** Update template request */
export type UpdateTemplateRequest = {
	name?: string;
	description?: string;
	graph?: WorkflowGraph;
};

/** Paginated response */
export type PaginatedResponse<T> = {
	items: T[];
	total: number;
	page: number;
	per_page: number;
};

/** Workflow instance */
export type WorkflowInstance = {
	id: string;
	template_id: string;
	template_version: number;
	net_id: string;
	status: 'created' | 'running' | 'completed' | 'failed' | 'cancelled';
	created_by: string;
	created_at: string;
	started_at: string | null;
	completed_at: string | null;
	current_step: string | null;
	metadata: Record<string, unknown>;
};

/** Create instance request */
export type CreateInstanceRequest = {
	template_id: string;
	created_by: string;
	metadata?: Record<string, unknown>;
};

/** Instance state from petri-lab */
export type InstanceState = {
	instance_id: string;
	net_id: string;
	status: 'running' | 'completed' | 'failed' | 'cancelled';
	marking: Record<string, Array<{ id: string; color: object }>>;
	enabled_transitions: string[];
	current_step?: string;
};

/** Compile preview result */
export type CompileResult = {
	air_json: object;
	errors: string[];
	warnings: string[];
};
