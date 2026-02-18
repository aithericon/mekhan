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
