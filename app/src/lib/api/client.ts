/**
 * Typed API client for mekhan-service.
 *
 * Function signatures and names match the hand-rolled client this replaces, so
 * existing call sites compile unchanged. Internally everything routes through
 * an `openapi-fetch` instance whose request/response types are derived from
 * `schema.d.ts` (regenerated from `openapi-mekhan.json` via
 * `npm run openapi:generate`).
 *
 * Types previously imported from `$lib/types/api` and friends now alias to
 * `components['schemas'][...]` so changes to the Rust DTOs flow into TS
 * automatically.
 */
import createClient, { type Middleware } from 'openapi-fetch';
import { auth } from '$lib/auth/store.svelte';
import { authFetch } from '$lib/auth/fetch';
import type { components, paths } from './schema';

const API_BASE = '/api';

/**
 * openapi-fetch middleware that injects the active Bearer token on every
 * request. Sourced from the auth store so silent renew and sign-out are
 * picked up automatically. In dev-noop mode the token is empty — the
 * backend's `NoopTokenVerifier` accepts that.
 */
const authMiddleware: Middleware = {
	async onRequest({ request }) {
		const token = auth.getAccessToken();
		if (token) {
			request.headers.set('Authorization', `Bearer ${token}`);
		}
		return request;
	}
};

const client = createClient<paths>({ baseUrl: '' });
client.use(authMiddleware);

// ── Type aliases (schema-driven) ───────────────────────────────────────────

// ─── Templates ──────────────────────────────────────────────────────────────
export type Template = components['schemas']['WorkflowTemplate'];
/** Backend returns full WorkflowTemplate rows; "summary" is a frontend label. */
export type TemplateSummary = Template;
export type CreateTemplateRequest = components['schemas']['CreateTemplateRequest'];
export type UpdateTemplateRequest = components['schemas']['UpdateTemplateRequest'];
export type CompileRequest = components['schemas']['CompileRequest'];
export type PaginatedTemplateResponse =
	components['schemas']['PaginatedResponse_WorkflowTemplate'];

// ─── Workflow graph (saved template body) ───────────────────────────────────
export type WorkflowGraph = components['schemas']['WorkflowGraph'];
export type WorkflowNode = components['schemas']['WorkflowNode'];
export type WorkflowNodeData = components['schemas']['WorkflowNodeData'];
export type WorkflowEdge = components['schemas']['WorkflowEdge'];
export type Viewport = components['schemas']['Viewport'];
export type Position = components['schemas']['Position'];
export type TaskStepConfig = components['schemas']['TaskStepConfig'];
export type TaskBlockConfig = components['schemas']['TaskBlockConfig'];
export type TaskFieldConfig = components['schemas']['TaskFieldConfig'];
export type BranchCondition = components['schemas']['BranchCondition'];
export type ExecutionSpecConfig = components['schemas']['ExecutionSpecConfig'];
// Enum aliases — schema-generated string literal unions kept in lockstep
// with the Rust `#[serde(rename_all = "snake_case")]` enums in
// `service/src/models/template.rs`.
export type CalloutSeverity = components['schemas']['CalloutSeverity'];
export type ImageDisplay = components['schemas']['ImageDisplay'];
export type TaskFieldKind = components['schemas']['TaskFieldKind'];
export type ExecutionBackendType = components['schemas']['ExecutionBackendType'];

// ─── Instances ──────────────────────────────────────────────────────────────
export type WorkflowInstance = components['schemas']['WorkflowInstance'];
export type InstanceListItem = components['schemas']['InstanceListItem'];
export type CreateInstanceRequest = components['schemas']['CreateInstanceRequest'];
export type InstanceStateResponse = components['schemas']['InstanceStateResponse'];

// ─── Processes / HPI ────────────────────────────────────────────────────────
export type HpiProcess = components['schemas']['HpiProcess'];
export type HpiTask = components['schemas']['HpiTask'];
export type HpiMetric = components['schemas']['HpiMetric'];
export type HpiMetricSummary = components['schemas']['HpiMetricSummary'];
export type HpiLog = components['schemas']['HpiLog'];
export type ProcessDetail = components['schemas']['ProcessDetail'];
export type ProcessStats = components['schemas']['ProcessStats'];

// ─── Catalogue ──────────────────────────────────────────────────────────────
export type CatalogueEntry = components['schemas']['CatalogueEntry'];
export type CatalogueStats = components['schemas']['CatalogueStats'];
export type CategoryStats = components['schemas']['CategoryStats'];
export type CatalogueNetStats = components['schemas']['NetStats'];
export type LineageResponse = components['schemas']['LineageResponse'];
export type LineageStep = components['schemas']['LineageStep'];

// ─── Provenance ─────────────────────────────────────────────────────────────
export type ProvenanceResponse = components['schemas']['ProvenanceResponse'];
export type AncestryNode = components['schemas']['AncestryNode'];
export type CrossNetEdge = components['schemas']['CrossNetEdge'];
export type CrossLink = components['schemas']['CrossLink'];
export type EventDetail = components['schemas']['EventDetail'];
export type TokenInfo = components['schemas']['TokenInfo'];
export type BridgeTarget = components['schemas']['BridgeTarget'];
export type SignalDispatch = components['schemas']['SignalDispatch'];

// ─── Live events / SSE payloads ─────────────────────────────────────────────
export type MetricsSeriesResponse = components['schemas']['MetricsSeriesResponse'];
export type MetricPoint = components['schemas']['MetricPoint'];
export type LiveMetricEvent = components['schemas']['LiveMetricEvent'];
export type LiveLogEvent = components['schemas']['LiveLogEvent'];
export type LiveArtifactEvent = components['schemas']['LiveArtifactEvent'];

// ─── Typed response envelopes ──────────────────────────────────────────────
export type InstanceEventsResponse = components['schemas']['InstanceEventsResponse'];
export type LogsTailResponse = components['schemas']['LogsTailResponse'];
export type ArtifactsListResponse = components['schemas']['ArtifactsListResponse'];
export type TaskListResponse = components['schemas']['TaskListResponse'];
export type FileUploadResponse = components['schemas']['FileUploadResponse'];
export type PaginatedProcesses = components['schemas']['Paginated_HpiProcess'];
export type PaginatedLogs = components['schemas']['Paginated_HpiLog'];
export type PaginatedArtifacts = components['schemas']['Paginated_CatalogueEntry'];

// Internal helper — `openapi-fetch` returns { data, error }. We surface the
// older "throws on non-2xx" contract so call sites don't need to change.
function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
	if (result.error !== undefined) {
		const status = result.response.status;
		const body =
			typeof result.error === 'object'
				? JSON.stringify(result.error)
				: String(result.error);
		throw new Error(`API error ${status}: ${body}`);
	}
	if (result.data === undefined) {
		throw new Error(`API error ${result.response.status}: empty body`);
	}
	return result.data;
}

// ── Templates ───────────────────────────────────────────────────────────────

export async function listTemplates(
	page = 1,
	perPage = 20,
	search?: string,
	published?: boolean
): Promise<PaginatedTemplateResponse> {
	return unwrap(
		await client.GET('/api/templates', {
			params: {
				query: { page, per_page: perPage, search, published }
			}
		})
	);
}

export async function getTemplate(id: string): Promise<Template> {
	return unwrap(
		await client.GET('/api/templates/{id}', { params: { path: { id } } })
	);
}

export async function createTemplate(data: CreateTemplateRequest): Promise<Template> {
	return unwrap(await client.POST('/api/templates', { body: data }));
}

export async function updateTemplate(id: string, data: UpdateTemplateRequest): Promise<Template> {
	return unwrap(
		await client.PUT('/api/templates/{id}', {
			params: { path: { id } },
			body: data
		})
	);
}

export async function deleteTemplate(id: string): Promise<void> {
	const res = await client.DELETE('/api/templates/{id}', { params: { path: { id } } });
	if (res.error !== undefined && res.response.status >= 400) {
		throw new Error(`API error ${res.response.status}: ${JSON.stringify(res.error)}`);
	}
}

/// Compile-error structured response from the workflow compiler. Maps to
/// `service::compiler::CompileErrorView`. Surfaced by `publishTemplate` via
/// `CompileApiError` when the publish handler returns a 400 with a body that
/// includes structured per-edge / per-node errors.
export type CompileErrorView = components['schemas']['CompileErrorView'];

export class CompileApiError extends Error {
	readonly compileErrors: CompileErrorView[];
	constructor(message: string, errors: CompileErrorView[]) {
		super(message);
		this.name = 'CompileApiError';
		this.compileErrors = errors;
	}
}

export async function publishTemplate(id: string): Promise<Template> {
	const res = await client.POST('/api/templates/{id}/publish', {
		params: { path: { id } }
	});
	const rawErr = res.error as unknown;
	if (rawErr !== undefined) {
		// Try to surface a structured compile failure so the editor can
		// highlight inline. Fall back to a generic Error otherwise.
		const body = rawErr as {
			error?: string;
			compile_errors?: CompileErrorView[] | null;
		};
		if (body && Array.isArray(body.compile_errors) && body.compile_errors.length > 0) {
			throw new CompileApiError(body.error ?? 'compilation failed', body.compile_errors);
		}
		const detail = typeof rawErr === 'object' ? JSON.stringify(rawErr) : String(rawErr);
		throw new Error(`API error ${res.response.status}: ${detail}`);
	}
	return res.data as Template;
}

export async function createNewVersion(id: string): Promise<Template> {
	return unwrap(
		await client.POST('/api/templates/{id}/new-version', { params: { path: { id } } })
	);
}

export async function getTemplateVersions(id: string): Promise<Template[]> {
	return unwrap(
		await client.GET('/api/templates/{id}/versions', { params: { path: { id } } })
	) as unknown as Template[];
}

export async function compileTemplate(id: string): Promise<object> {
	return unwrap(
		await client.POST('/api/templates/{id}/compile', { params: { path: { id } } })
	) as unknown as object;
}

export async function getTemplateAir(id: string): Promise<object> {
	return unwrap(
		await client.GET('/api/templates/{id}/air', { params: { path: { id } } })
	) as unknown as object;
}

export async function compileGraph(data: CompileRequest): Promise<object> {
	return unwrap(await client.POST('/api/compile', { body: data })) as unknown as object;
}

/**
 * Generated `_aithericon_io` files per Python automated step, keyed
 * `nodeId -> { filename -> source }` — a `.py` SDK delegate plus a typed
 * `.pyi` overlay. Authoring aid: works on non-publishable graphs and never
 * errors (empty map if unscopable), so the IDE can surface read-only typed
 * `load_input()` stubs before publish.
 */
export async function getIoStubs(
	id: string
): Promise<Record<string, Record<string, string>>> {
	const res = unwrap(
		await client.GET('/api/templates/{id}/io-stubs', { params: { path: { id } } })
	) as { generated?: Record<string, Record<string, string>> };
	return res.generated ?? {};
}

/** A field readable as `token.<name>` at a Python step (its input scope). */
export type StepScopeField = { name: string; kind: string };

/**
 * Per-node input scope plus a `diagnostic` explaining the result so an empty
 * panel can say *why* (live graph unreadable, graph not yet a DAG, or simply
 * no upstream) instead of looking broken. `scopes` is keyed
 * `nodeId -> [{ name, kind }]` — the same `input.<field>` set the generated
 * `.pyi` types and decision guards see. Never throws: failures degrade to an
 * empty map with a diagnostic.
 */
export type StepScopes = {
	scopes: Record<string, StepScopeField[]>;
	diagnostic: string;
};

export async function getStepScopes(id: string): Promise<StepScopes> {
	try {
		const res = unwrap(
			await client.GET('/api/templates/{id}/io-stubs', { params: { path: { id } } })
		) as { scopes?: Record<string, StepScopeField[]>; diagnostic?: string };
		return { scopes: res.scopes ?? {}, diagnostic: res.diagnostic ?? 'ok' };
	} catch (e) {
		return { scopes: {}, diagnostic: `request_failed: ${e instanceof Error ? e.message : e}` };
	}
}

// ── Instances ───────────────────────────────────────────────────────────────

export async function listInstances(
	page = 1,
	perPage = 20
): Promise<components['schemas']['PaginatedResponse_InstanceListItem']> {
	return unwrap(
		await client.GET('/api/instances', {
			params: { query: { page, per_page: perPage } }
		})
	);
}

export async function getInstance(id: string): Promise<WorkflowInstance> {
	return unwrap(
		await client.GET('/api/instances/{id}', { params: { path: { id } } })
	);
}

export async function createInstance(data: CreateInstanceRequest): Promise<WorkflowInstance> {
	return unwrap(await client.POST('/api/instances', { body: data }));
}

export async function getInstanceState(id: string): Promise<InstanceStateResponse> {
	return unwrap(
		await client.GET('/api/instances/{id}/state', { params: { path: { id } } })
	);
}

export async function cancelInstance(id: string): Promise<void> {
	const res = await client.DELETE('/api/instances/{id}', { params: { path: { id } } });
	if (res.error !== undefined && res.response.status >= 400) {
		throw new Error(`API error ${res.response.status}: ${JSON.stringify(res.error)}`);
	}
}

// ── Tasks (HpiTask is wire shape; HumanTask is the richer hpi-ui type) ─────

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
	return rawJson(`/tasks${query ? `?${query}` : ''}`);
}

export async function getTask(taskId: string): Promise<import('$lib/types/tasks').HumanTask> {
	return rawJson(`/tasks/${taskId}`);
}

export async function completeTask(
	taskId: string,
	data: Record<string, unknown>
): Promise<void> {
	await rawJson(`/tasks/${taskId}/complete`, {
		method: 'POST',
		body: JSON.stringify({ data })
	});
}

export async function cancelTask(taskId: string, reason?: string): Promise<void> {
	await rawJson(`/tasks/${taskId}/cancel`, {
		method: 'POST',
		body: JSON.stringify({ reason })
	});
}

// ── Processes ───────────────────────────────────────────────────────────────

export async function listProcesses(params?: {
	status?: string;
	kind?: string;
	search?: string;
	sort?: string;
	page?: number;
	page_size?: number;
}): Promise<import('$lib/types/process').PaginatedProcessResponse<HpiProcess>> {
	const qs = new URLSearchParams();
	if (params?.status) qs.set('status', params.status);
	if (params?.kind) qs.set('kind', params.kind);
	if (params?.search) qs.set('search', params.search);
	if (params?.sort) qs.set('sort', params.sort);
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	const query = qs.toString();
	return rawJson(`/processes${query ? `?${query}` : ''}`);
}

export async function getProcessStats(): Promise<ProcessStats> {
	return unwrap(await client.GET('/api/processes/stats', {}));
}

export async function getProcess(processId: string): Promise<ProcessDetail> {
	return unwrap(
		await client.GET('/api/processes/{process_id}', { params: { path: { process_id: processId } } })
	);
}

export async function updateProcess(
	processId: string,
	data: { name?: string; kind?: string; status?: string; owner?: string }
): Promise<HpiProcess> {
	return unwrap(
		await client.PUT('/api/processes/{process_id}', {
			params: { path: { process_id: processId } },
			body: data
		})
	);
}

export async function getProcessMetrics(
	processId: string,
	params?: { key?: string; limit?: number }
): Promise<HpiMetric[]> {
	return unwrap(
		await client.GET('/api/processes/{process_id}/metrics', {
			params: { path: { process_id: processId }, query: params }
		})
	);
}

export async function getProcessMetricsSummary(processId: string): Promise<HpiMetricSummary[]> {
	return unwrap(
		await client.GET('/api/processes/{process_id}/metrics/summary', {
			params: { path: { process_id: processId } }
		})
	);
}

export async function getProcessLogs(
	processId: string,
	params?: { level?: string; source?: string; search?: string; page?: number; page_size?: number }
): Promise<import('$lib/types/process').PaginatedProcessResponse<HpiLog>> {
	const qs = new URLSearchParams();
	if (params?.level) qs.set('level', params.level);
	if (params?.source) qs.set('source', params.source);
	if (params?.search) qs.set('search', params.search);
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	const query = qs.toString();
	return rawJson(`/processes/${processId}/logs${query ? `?${query}` : ''}`);
}

export async function getProcessTasks(processId: string): Promise<HpiTask[]> {
	return unwrap(
		await client.GET('/api/processes/{process_id}/tasks', {
			params: { path: { process_id: processId } }
		})
	) as unknown as HpiTask[];
}

// ── Live metrics / logs (SSE + DB backfill) ─────────────────────────────────

export interface LogTailRow {
	id: number;
	process_id: string;
	level: string;
	source: string | null;
	message: string;
	detail: Record<string, unknown> | null;
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
	return rawJson(`/processes/${processId}/metrics/series${query ? `?${query}` : ''}`);
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
): Promise<LogsTailResponse> {
	const qs = new URLSearchParams();
	if (params?.since) qs.set('since', toIso(params.since));
	if (params?.until) qs.set('until', toIso(params.until));
	if (params?.level) qs.set('level', params.level);
	if (params?.signal_key) qs.set('signal_key', params.signal_key);
	if (params?.q) qs.set('q', params.q);
	if (params?.limit) qs.set('limit', String(params.limit));
	const query = qs.toString();
	return rawJson(`/processes/${processId}/logs/tail${query ? `?${query}` : ''}`);
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
): Promise<ArtifactsListResponse> {
	const qs = new URLSearchParams();
	if (params?.categories && params.categories.length > 0)
		qs.set('categories', params.categories.join(','));
	if (params?.render_hints && params.render_hints.length > 0)
		qs.set('render_hints', params.render_hints.join(','));
	if (params?.since) qs.set('since', toIso(params.since));
	if (params?.until) qs.set('until', toIso(params.until));
	if (params?.limit) qs.set('limit', String(params.limit));
	const query = qs.toString();
	return rawJson(`/processes/${processId}/artifacts/list${query ? `?${query}` : ''}`);
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
): Promise<import('$lib/types/process').PaginatedProcessResponse<CatalogueEntry>> {
	const qs = new URLSearchParams();
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	const query = qs.toString();
	return rawJson(`/processes/${processId}/artifacts${query ? `?${query}` : ''}`);
}

// ── Catalogue ───────────────────────────────────────────────────────────────

/** Pagination envelope for `GET /api/catalogue` — the backend returns this
 *  shape through its custom query-DSL repository, but the spec types the
 *  response as `serde_json::Value` since the DSL isn't modeled in utoipa. */
export interface PaginatedCatalogueResponse {
	items: CatalogueEntry[];
	total: number;
	page: number;
	page_size: number;
	total_pages: number;
	has_next: boolean;
	has_previous: boolean;
}

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
	return rawJson(`/catalogue${query ? `?${query}` : ''}`);
}

export async function getCatalogueEntry(executionId: string, id: string): Promise<CatalogueEntry> {
	return unwrap(
		await client.GET('/api/catalogue/{execution_id}/{id}', {
			params: { path: { execution_id: executionId, id } }
		})
	);
}

export async function getCatalogueStats(): Promise<CatalogueStats> {
	return unwrap(await client.GET('/api/catalogue/stats', {}));
}

export async function getCatalogueStatsByNet(): Promise<CatalogueNetStats[]> {
	return unwrap(await client.GET('/api/catalogue/stats/by-net', {}));
}

export async function getCatalogueLineage(processId: string): Promise<LineageResponse> {
	return unwrap(
		await client.GET('/api/catalogue/lineage/{process_id}', {
			params: { path: { process_id: processId } }
		})
	);
}

export async function getCatalogueDistinct(column: string): Promise<string[]> {
	return unwrap(
		await client.GET('/api/catalogue/distinct/{column}', { params: { path: { column } } })
	);
}

export async function getCatalogueDistinctJsonb(column: string, key: string): Promise<string[]> {
	return unwrap(
		await client.GET('/api/catalogue/distinct-jsonb/{column}/{key}', {
			params: { path: { column, key } }
		})
	);
}

export function catalogueDownloadUrl(storagePath: string): string {
	return `${API_BASE}/catalogue/download/${storagePath}`;
}

// ── File upload (multipart — direct fetch since openapi-fetch doesn't help here) ─

export async function uploadFile(
	templateId: string,
	nodeId: string,
	file: File
): Promise<FileUploadResponse> {
	const formData = new FormData();
	formData.append('file', file);

	const res = await authFetch(`${API_BASE}/files/upload/${templateId}/${nodeId}`, {
		method: 'POST',
		body: formData
	});

	if (!res.ok) {
		const body = await res.text();
		throw new Error(`Upload error ${res.status}: ${body}`);
	}

	return res.json();
}

// ── Provenance ──────────────────────────────────────────────────────────────

export async function getProvenanceFromArtifact(
	executionId: string,
	artifactId: string,
	depth = 20
): Promise<ProvenanceResponse> {
	return unwrap(
		await client.GET('/api/provenance/from-artifact/{execution_id}/{artifact_id}', {
			params: { path: { execution_id: executionId, artifact_id: artifactId }, query: { depth } }
		})
	);
}

export async function getTokenProvenance(
	netId: string,
	tokenId: string,
	depth = 20
): Promise<ProvenanceResponse> {
	return unwrap(
		await client.GET('/api/provenance/{net_id}/{token_id}', {
			params: { path: { net_id: netId, token_id: tokenId }, query: { depth } }
		})
	);
}

export async function getEventDetail(netId: string, eventSeq: number): Promise<EventDetail> {
	return unwrap(
		await client.GET('/api/provenance/{net_id}/{event_seq}/detail', {
			params: { path: { net_id: netId, event_seq: eventSeq } }
		})
	);
}

export async function getCrossLink(signalKey: string): Promise<CrossLink> {
	return unwrap(
		await client.GET('/api/provenance/link/{signal_key}', {
			params: { path: { signal_key: signalKey } }
		})
	);
}

// ── Untyped raw-JSON helper ────────────────────────────────────────────────
//
// A handful of routes return `serde_json::Value` envelopes
// (`{ tasks }`, `{ entries }`, custom query DSL responses on processes/logs/
// artifacts/catalogue list, plus the HPI-task-shaped tasks endpoints). openapi-fetch
// can't help here — the schema says "object" with no fields. Until those
// responses are typed on the backend, fall back to plain fetch with the call
// site asserting the shape.
async function rawJson<T>(path: string, init?: RequestInit): Promise<T> {
	const res = await authFetch(`${API_BASE}${path}`, {
		...init,
		headers: { 'Content-Type': 'application/json', ...init?.headers }
	});
	if (!res.ok) {
		const body = await res.text();
		throw new Error(`API error ${res.status}: ${body}`);
	}
	return res.json() as Promise<T>;
}
