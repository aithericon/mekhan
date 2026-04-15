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
import type {
	HpiProcess,
	HpiTask,
	HpiMetric,
	HpiMetricSummary,
	HpiLog,
	ProcessDetail,
	ProcessStats,
	PaginatedProcessResponse
} from '$lib/types/process';
import type { ProvenanceResponse, EventDetail, CrossLink } from '$lib/types/provenance';

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

// Process endpoints (Mekhan process tracking)
export async function listProcesses(params?: {
	status?: string;
	kind?: string;
	search?: string;
	sort?: string;
	page?: number;
	page_size?: number;
}): Promise<PaginatedProcessResponse<HpiProcess>> {
	const qs = new URLSearchParams();
	if (params?.status) qs.set('status', params.status);
	if (params?.kind) qs.set('kind', params.kind);
	if (params?.search) qs.set('search', params.search);
	if (params?.sort) qs.set('sort', params.sort);
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	const query = qs.toString();
	return request(`/processes${query ? `?${query}` : ''}`);
}

export async function getProcessStats(): Promise<ProcessStats> {
	return request('/processes/stats');
}

export async function getProcess(processId: string): Promise<ProcessDetail> {
	return request(`/processes/${processId}`);
}

export async function updateProcess(
	processId: string,
	data: { name?: string; kind?: string; status?: string; owner?: string }
): Promise<HpiProcess> {
	return request(`/processes/${processId}`, {
		method: 'PUT',
		body: JSON.stringify(data)
	});
}

export async function getProcessMetrics(
	processId: string,
	params?: { key?: string; limit?: number }
): Promise<HpiMetric[]> {
	const qs = new URLSearchParams();
	if (params?.key) qs.set('key', params.key);
	if (params?.limit) qs.set('limit', String(params.limit));
	const query = qs.toString();
	return request(`/processes/${processId}/metrics${query ? `?${query}` : ''}`);
}

export async function getProcessMetricsSummary(
	processId: string
): Promise<HpiMetricSummary[]> {
	return request(`/processes/${processId}/metrics/summary`);
}

export async function getProcessLogs(
	processId: string,
	params?: { level?: string; source?: string; search?: string; page?: number; page_size?: number }
): Promise<PaginatedProcessResponse<HpiLog>> {
	const qs = new URLSearchParams();
	if (params?.level) qs.set('level', params.level);
	if (params?.source) qs.set('source', params.source);
	if (params?.search) qs.set('search', params.search);
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	const query = qs.toString();
	return request(`/processes/${processId}/logs${query ? `?${query}` : ''}`);
}

export async function getProcessTasks(processId: string): Promise<HpiTask[]> {
	return request(`/processes/${processId}/tasks`);
}

// ── Live metrics / logs (SSE + DB backfill) ─────────────────────────────────

export interface MetricPoint {
	t: string;
	v: number;
}

export interface MetricsSeriesResponse {
	bucket_seconds: number;
	series: Record<string, MetricPoint[]>;
}

export interface LiveMetricEvent {
	seq: number;
	process_id: string;
	signal_key: string | null;
	key: string;
	value: number;
	timestamp: string;
}

export interface LiveLogEvent {
	seq: number;
	process_id: string;
	signal_key: string | null;
	level: string;
	source: string | null;
	message: string;
	detail: unknown;
	timestamp: string;
}

export interface LogTailRow {
	id: number;
	process_id: string;
	level: string;
	source: string | null;
	message: string;
	detail: unknown;
	timestamp: string;
	signal_key: string | null;
}

export async function getProcessMetricsSeries(
	processId: string,
	params: {
		keys?: string[];
		since?: Date | string;
		until?: Date | string;
		signal_key?: string;
		max_points?: number;
	}
): Promise<MetricsSeriesResponse> {
	const qs = new URLSearchParams();
	if (params.keys && params.keys.length > 0) qs.set('keys', params.keys.join(','));
	if (params.since) qs.set('since', toIso(params.since));
	if (params.until) qs.set('until', toIso(params.until));
	if (params.signal_key) qs.set('signal_key', params.signal_key);
	if (params.max_points) qs.set('max_points', String(params.max_points));
	const query = qs.toString();
	return request(`/processes/${processId}/metrics/series${query ? `?${query}` : ''}`);
}

export async function getProcessLogsTail(
	processId: string,
	params?: {
		since?: Date | string;
		until?: Date | string;
		level?: string;
		signal_key?: string;
		q?: string;
		limit?: number;
	}
): Promise<{ logs: LogTailRow[] }> {
	const qs = new URLSearchParams();
	if (params?.since) qs.set('since', toIso(params.since));
	if (params?.until) qs.set('until', toIso(params.until));
	if (params?.level) qs.set('level', params.level);
	if (params?.signal_key) qs.set('signal_key', params.signal_key);
	if (params?.q) qs.set('q', params.q);
	if (params?.limit) qs.set('limit', String(params.limit));
	const query = qs.toString();
	return request(`/processes/${processId}/logs/tail${query ? `?${query}` : ''}`);
}

export function processMetricsStreamUrl(
	processId: string,
	params: { since_seq?: number; signal_key?: string; keys?: string[] } = {}
): string {
	const qs = new URLSearchParams();
	if (params.since_seq !== undefined) qs.set('since_seq', String(params.since_seq));
	if (params.signal_key) qs.set('signal_key', params.signal_key);
	if (params.keys && params.keys.length > 0) qs.set('keys', params.keys.join(','));
	const query = qs.toString();
	return `${API_BASE}/processes/${processId}/metrics/stream${query ? `?${query}` : ''}`;
}

export function processLogsStreamUrl(
	processId: string,
	params: { since_seq?: number; signal_key?: string; level?: string; q?: string } = {}
): string {
	const qs = new URLSearchParams();
	if (params.since_seq !== undefined) qs.set('since_seq', String(params.since_seq));
	if (params.signal_key) qs.set('signal_key', params.signal_key);
	if (params.level) qs.set('level', params.level);
	if (params.q) qs.set('q', params.q);
	const query = qs.toString();
	return `${API_BASE}/processes/${processId}/logs/stream${query ? `?${query}` : ''}`;
}

// ── Artifacts (live) ────────────────────────────────────────────────────────

export interface LiveArtifactEntry {
	seq?: number; // present on live events, absent on DB-backfill rows
	process_id?: string;
	artifact_id?: string;
	id?: string; // DB rows use `id`; SSE uses `artifact_id`
	execution_id: string;
	name: string;
	category: string;
	filename: string;
	mime_type: string | null;
	storage_path: string | null;
	size_bytes: number | null;
	process_step: string | null;
	signal_key: string | null;
	user_metadata: Record<string, unknown> | null;
	created_at: string;
}

export async function getProcessArtifactsList(
	processId: string,
	params?: {
		categories?: string[];
		render_hints?: string[];
		since?: Date | string;
		until?: Date | string;
		limit?: number;
	}
): Promise<{ entries: LiveArtifactEntry[] }> {
	const qs = new URLSearchParams();
	if (params?.categories && params.categories.length > 0)
		qs.set('categories', params.categories.join(','));
	if (params?.render_hints && params.render_hints.length > 0)
		qs.set('render_hints', params.render_hints.join(','));
	if (params?.since) qs.set('since', toIso(params.since));
	if (params?.until) qs.set('until', toIso(params.until));
	if (params?.limit) qs.set('limit', String(params.limit));
	const query = qs.toString();
	return request(
		`/processes/${processId}/artifacts/list${query ? `?${query}` : ''}`
	);
}

export function processArtifactsStreamUrl(
	processId: string,
	params: { since_seq?: number; categories?: string[]; render_hints?: string[] } = {}
): string {
	const qs = new URLSearchParams();
	if (params.since_seq !== undefined) qs.set('since_seq', String(params.since_seq));
	if (params.categories && params.categories.length > 0)
		qs.set('categories', params.categories.join(','));
	if (params.render_hints && params.render_hints.length > 0)
		qs.set('render_hints', params.render_hints.join(','));
	const query = qs.toString();
	return `${API_BASE}/processes/${processId}/artifacts/stream${query ? `?${query}` : ''}`;
}

function toIso(d: Date | string): string {
	return typeof d === 'string' ? d : d.toISOString();
}

export async function getProcessArtifacts(
	processId: string,
	params?: { page?: number; page_size?: number }
): Promise<PaginatedProcessResponse<CatalogueEntry>> {
	const qs = new URLSearchParams();
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	const query = qs.toString();
	return request(`/processes/${processId}/artifacts${query ? `?${query}` : ''}`);
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

// ── Provenance ────────────────────────────────────────────────────────────

export async function getProvenanceFromArtifact(
	executionId: string,
	artifactId: string,
	depth = 20
): Promise<ProvenanceResponse> {
	return request(`/provenance/from-artifact/${encodeURIComponent(executionId)}/${encodeURIComponent(artifactId)}?depth=${depth}`);
}

export async function getTokenProvenance(
	netId: string,
	tokenId: string,
	depth = 20
): Promise<ProvenanceResponse> {
	return request(`/provenance/${netId}/${tokenId}?depth=${depth}`);
}

export async function getEventDetail(
	netId: string,
	eventSeq: number
): Promise<EventDetail> {
	return request(`/provenance/${netId}/${eventSeq}/detail`);
}

export async function getCrossLink(signalKey: string): Promise<CrossLink> {
	return request(`/provenance/link/${signalKey}`);
}
