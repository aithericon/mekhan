import type {
	Template,
	TemplateSummary,
	CreateTemplateRequest,
	UpdateTemplateRequest,
	PaginatedResponse,
	WorkflowInstance,
	CreateInstanceRequest,
	InstanceState,
	CompileResult
} from '$lib/types/api';
import type { WorkflowGraph } from '$lib/types/editor';
import type {
	CatalogueEntry,
	PaginatedCatalogueResponse,
	CatalogueStats,
	CatalogueNetStats,
	LineageResponse
} from '$lib/types/catalogue';

const API_BASE = '/api';

async function request<T>(path: string, init?: RequestInit): Promise<T> {
	const res = await fetch(`${API_BASE}${path}`, {
		...init,
		headers: {
			'Content-Type': 'application/json',
			...init?.headers
		}
	});

	if (!res.ok) {
		const body = await res.text();
		throw new Error(`API error ${res.status}: ${body}`);
	}

	return res.json();
}

// Template endpoints
export async function listTemplates(
	page = 1,
	perPage = 20,
	search?: string,
	published?: boolean
): Promise<PaginatedResponse<TemplateSummary>> {
	const params = new URLSearchParams({ page: String(page), per_page: String(perPage) });
	if (search) params.set('search', search);
	if (published !== undefined) params.set('published', String(published));
	return request(`/templates?${params}`);
}

export async function getTemplate(id: string): Promise<Template> {
	return request(`/templates/${id}`);
}

export async function createTemplate(data: CreateTemplateRequest): Promise<Template> {
	return request('/templates', {
		method: 'POST',
		body: JSON.stringify(data)
	});
}

export async function updateTemplate(id: string, data: UpdateTemplateRequest): Promise<Template> {
	return request(`/templates/${id}`, {
		method: 'PUT',
		body: JSON.stringify(data)
	});
}

export async function deleteTemplate(id: string): Promise<void> {
	await request(`/templates/${id}`, { method: 'DELETE' });
}

export async function publishTemplate(id: string): Promise<Template> {
	return request(`/templates/${id}/publish`, { method: 'POST' });
}

export async function createNewVersion(id: string): Promise<Template> {
	return request(`/templates/${id}/new-version`, { method: 'POST' });
}

export async function getTemplateVersions(id: string): Promise<Template[]> {
	return request(`/templates/${id}/versions`);
}

export async function compileTemplate(id: string): Promise<CompileResult> {
	return request(`/templates/${id}/compile`, { method: 'POST' });
}

export async function getTemplateAir(id: string): Promise<object> {
	return request(`/templates/${id}/air`);
}

// Compile graph (without saving as template)
export async function compileGraph(data: {
	name: string;
	description?: string;
	graph: WorkflowGraph;
}): Promise<object> {
	return request('/compile', { method: 'POST', body: JSON.stringify(data) });
}

// Instance endpoints
export async function listInstances(
	page = 1,
	perPage = 20
): Promise<PaginatedResponse<WorkflowInstance>> {
	const params = new URLSearchParams({ page: String(page), per_page: String(perPage) });
	return request(`/instances?${params}`);
}

export async function getInstance(id: string): Promise<WorkflowInstance> {
	return request(`/instances/${id}`);
}

export async function createInstance(data: CreateInstanceRequest): Promise<WorkflowInstance> {
	return request('/instances', {
		method: 'POST',
		body: JSON.stringify(data)
	});
}

export async function getInstanceState(id: string): Promise<InstanceState> {
	return request(`/instances/${id}/state`);
}

export async function cancelInstance(id: string): Promise<void> {
	await request(`/instances/${id}`, { method: 'DELETE' });
}

// Task endpoints (proxied to HPI)
export async function listTasks(params?: {
	status?: string;
	search?: string;
	process_id?: string;
	limit?: number;
	offset?: number;
}): Promise<{ tasks: import('$lib/types/tasks').HumanTask[]; total: number }> {
	const qs = new URLSearchParams();
	if (params?.status) qs.set('status', params.status);
	if (params?.search) qs.set('search', params.search);
	if (params?.process_id) qs.set('process_id', params.process_id);
	if (params?.limit) qs.set('limit', String(params.limit));
	if (params?.offset) qs.set('offset', String(params.offset));
	const query = qs.toString();
	return request(`/tasks${query ? `?${query}` : ''}`);
}

export async function getTask(taskId: string): Promise<import('$lib/types/tasks').HumanTask> {
	return request(`/tasks/${taskId}`);
}

export async function completeTask(
	taskId: string,
	data: Record<string, unknown>
): Promise<void> {
	await request(`/tasks/${taskId}/complete`, {
		method: 'POST',
		body: JSON.stringify({ data })
	});
}

export async function cancelTask(taskId: string, reason?: string): Promise<void> {
	await request(`/tasks/${taskId}/cancel`, {
		method: 'POST',
		body: JSON.stringify({ reason })
	});
}

// Process endpoints (proxied to HPI)
export async function listProcesses(params?: {
	status?: string;
	namespace?: string;
	search?: string;
	limit?: number;
	offset?: number;
}): Promise<{ processes: import('$lib/types/tasks').ProcessState[]; total: number }> {
	const qs = new URLSearchParams();
	if (params?.status) qs.set('status', params.status);
	if (params?.namespace) qs.set('namespace', params.namespace);
	if (params?.search) qs.set('search', params.search);
	if (params?.limit) qs.set('limit', String(params.limit));
	if (params?.offset) qs.set('offset', String(params.offset));
	const query = qs.toString();
	return request(`/processes${query ? `?${query}` : ''}`);
}

export async function getProcess(
	processId: string
): Promise<import('$lib/types/tasks').ProcessState> {
	return request(`/processes/${processId}`);
}

// Catalogue endpoints
export async function listCatalogueEntries(params?: {
	category?: string;
	source_net?: string;
	process_id?: string;
	search?: string;
	sort?: string;
	page?: number;
	page_size?: number;
	metadata?: string;
	file_metadata?: string;
}): Promise<PaginatedCatalogueResponse> {
	const qs = new URLSearchParams();
	if (params?.category) qs.set('filter[category][eq]', params.category);
	if (params?.source_net) qs.set('filter[source_net][eq]', params.source_net);
	if (params?.process_id) qs.set('filter[process_id][eq]', params.process_id);
	if (params?.search) qs.set('search', params.search);
	if (params?.sort) qs.set('sort', params.sort);
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	if (params?.metadata) qs.set('metadata', params.metadata);
	if (params?.file_metadata) qs.set('file_metadata', params.file_metadata);
	const query = qs.toString();
	return request(`/catalogue${query ? `?${query}` : ''}`);
}

export async function getCatalogueEntry(executionId: string, id: string): Promise<CatalogueEntry> {
	return request(`/catalogue/${executionId}/${id}`);
}

export async function getCatalogueStats(): Promise<CatalogueStats> {
	return request('/catalogue/stats');
}

export async function getCatalogueStatsByNet(): Promise<CatalogueNetStats[]> {
	return request('/catalogue/stats/by-net');
}

export async function getCatalogueLineage(processId: string): Promise<LineageResponse> {
	return request(`/catalogue/lineage/${processId}`);
}

export async function getCatalogueDistinct(column: string): Promise<string[]> {
	return request(`/catalogue/distinct/${column}`);
}

export async function getCatalogueDistinctJsonb(column: string, key: string): Promise<string[]> {
	return request(`/catalogue/distinct-jsonb/${column}/${key}`);
}

export function catalogueDownloadUrl(storagePath: string): string {
	return `${API_BASE}/catalogue/download/${storagePath}`;
}

// File upload
export async function uploadFile(
	templateId: string,
	nodeId: string,
	file: File
): Promise<{ key: string; filename: string; content_type: string; size: number }> {
	const formData = new FormData();
	formData.append('file', file);

	const res = await fetch(`${API_BASE}/files/upload/${templateId}/${nodeId}`, {
		method: 'POST',
		body: formData
	});

	if (!res.ok) {
		const body = await res.text();
		throw new Error(`Upload error ${res.status}: ${body}`);
	}

	return res.json();
}
