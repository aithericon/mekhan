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
import { authFetch } from '$lib/auth/fetch';
import type { components, paths } from './schema';

const API_BASE = '/api/v1';

/**
 * BFF model: the `mekhan_session` HttpOnly cookie is sent automatically on
 * every same-origin request, so there is no Bearer to inject. This middleware
 * only handles the unauthenticated case: a 401 from any API call means the
 * session is gone — bounce (full-page) to the server-side login so Zitadel
 * can re-establish it. The `/api/auth/*` endpoints are exempt (they 401 *by
 * design* as the signed-out probe).
 */
const sessionExpiryMiddleware: Middleware = {
	async onResponse({ response, request }) {
		if (
			response.status === 401 &&
			typeof window !== 'undefined' &&
			!new URL(request.url).pathname.startsWith('/api/auth/')
		) {
			const here = window.location.pathname + window.location.search;
			window.location.assign(`/api/auth/login?return_to=${encodeURIComponent(here)}`);
		}
		return response;
	}
};

const client = createClient<paths>({
	baseUrl: '',
	// Send the session cookie on same-origin API calls.
	credentials: 'same-origin'
});
client.use(sessionExpiryMiddleware);

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

// ─── Template tests ─────────────────────────────────────────────────────────
export type TemplateTest = components['schemas']['TemplateTest'];
export type TemplateTestRun = components['schemas']['TemplateTestRun'];
export type Assertion = components['schemas']['Assertion'];
export type AssertOp = components['schemas']['AssertOp'];
export type CreateTemplateTestRequest =
	components['schemas']['CreateTemplateTestRequest'];
export type UpdateTemplateTestRequest =
	components['schemas']['UpdateTemplateTestRequest'];
export type RunAllResponse = components['schemas']['RunAllResponse'];
export type FailingTestInfo = components['schemas']['FailingTestInfo'];
export type PromoteToTestRequest = components['schemas']['PromoteToTestRequest'];

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
export type StepExecution = components['schemas']['StepExecutionResponse'];

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

// ─── Access tokens (embedded PAT management) ────────────────────────────────
export type TokenSummary = components['schemas']['TokenSummary'];
export type CreatedToken = components['schemas']['CreatedToken'];
export type CreateTokenRequest = components['schemas']['CreateTokenRequest'];

// ─── Workspaces, projects, tags, visibility (Phase A2 + B) ─────────────────
export type WorkspaceSummary = components['schemas']['WorkspaceSummary'];
export type WorkspaceMember = components['schemas']['WorkspaceMember'];
export type AddMemberRequest = components['schemas']['AddMemberRequest'];
export type Project = components['schemas']['Project'];
export type CreateProjectRequest = components['schemas']['CreateProjectRequest'];
export type AttachTemplateRequest = components['schemas']['AttachTemplateRequest'];
export type SetTagsRequest = components['schemas']['SetTagsRequest'];
export type SetVisibilityRequest = components['schemas']['SetVisibilityRequest'];
export type SetActiveWorkspaceRequest =
	components['schemas']['SetActiveWorkspaceRequest'];
export type ResolveEmailRequest = components['schemas']['ResolveEmailRequest'];
export type ResolveEmailResponse = components['schemas']['ResolveEmailResponse'];

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

/// Error thrown by `unwrap` (and `rawJson`) when the API returns a non-2xx
/// status. Carries the structured `ErrorResponse` envelope so callers can
/// switch on `code` for programmatic handling rather than parsing the
/// human-readable `message`.
export class ApiError extends Error {
	readonly status: number;
	readonly code: string | undefined;
	readonly body: { error?: string; code?: string; [k: string]: unknown };
	constructor(status: number, body: Record<string, unknown> | string | undefined) {
		const envelope =
			typeof body === 'object' && body !== null
				? (body as { error?: string; code?: string })
				: { error: typeof body === 'string' ? body : undefined };
		const message = envelope.error ?? `API error ${status}`;
		super(`API error ${status}: ${message}`);
		this.name = 'ApiError';
		this.status = status;
		this.code = envelope.code;
		this.body = envelope as { error?: string; code?: string };
	}
}

// Internal helper — `openapi-fetch` returns { data, error }. We surface the
// older "throws on non-2xx" contract so call sites don't need to change.
function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
	if (result.error !== undefined) {
		throw new ApiError(
			result.response.status,
			result.error as Record<string, unknown> | string | undefined
		);
	}
	if (result.data === undefined) {
		throw new ApiError(result.response.status, 'empty body');
	}
	return result.data;
}

// ── Templates ───────────────────────────────────────────────────────────────

export async function listTemplates(
	page = 1,
	perPage = 20,
	search?: string,
	published?: boolean,
	projectId?: string,
	tag?: string
): Promise<PaginatedTemplateResponse> {
	return unwrap(
		await client.GET('/api/v1/templates', {
			params: {
				query: {
					page,
					per_page: perPage,
					search,
					published,
					project_id: projectId,
					tag
				}
			}
		})
	);
}

export async function getTemplate(id: string): Promise<Template> {
	return unwrap(
		await client.GET('/api/v1/templates/{id}', { params: { path: { id } } })
	);
}

export async function createTemplate(data: CreateTemplateRequest): Promise<Template> {
	return unwrap(await client.POST('/api/v1/templates', { body: data }));
}

export async function updateTemplate(id: string, data: UpdateTemplateRequest): Promise<Template> {
	return unwrap(
		await client.PUT('/api/v1/templates/{id}', {
			params: { path: { id } },
			body: data
		})
	);
}

export async function deleteTemplate(id: string): Promise<void> {
	const res = await client.DELETE('/api/v1/templates/{id}', { params: { path: { id } } });
	if (res.error !== undefined && res.response.status >= 400) {
		throw new ApiError(
			res.response.status,
			res.error as Record<string, unknown> | string | undefined
		);
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

/// Thrown when the publish gate (412) blocks a publish because tests are
/// failing or stale. The editor catches this to render the gate modal with
/// per-test detail.
export class PublishGateError extends Error {
	readonly failingTests: FailingTestInfo[];
	constructor(message: string, failingTests: FailingTestInfo[]) {
		super(message);
		this.name = 'PublishGateError';
		this.failingTests = failingTests;
	}
}

export async function publishTemplate(id: string, force = false): Promise<Template> {
	// Coerce explicitly: callers that wire this as a DOM event handler
	// (e.g. `onclick={() => publishTemplate(id)}`) may accidentally let an
	// Event object land here. openapi-fetch's default querySerializer
	// rejects non-primitive query values with the unhelpful
	// "Deeply-nested arrays/objects aren't supported".
	const forceBool = force === true;
	const res = await client.POST('/api/v1/templates/{id}/publish', {
		params: { path: { id }, query: { force: forceBool } }
	});
	const rawErr = res.error as unknown;
	if (rawErr !== undefined) {
		// Three failure modes to disentangle from the same `error` shape:
		// 412 → test gate (has `failing_tests`); 400 with compile_errors → compiler;
		// anything else → generic.
		const body = rawErr as {
			error?: string;
			compile_errors?: CompileErrorView[] | null;
			failing_tests?: FailingTestInfo[] | null;
		};
		if (res.response.status === 412 && Array.isArray(body.failing_tests)) {
			throw new PublishGateError(
				body.error ?? 'template tests failing',
				body.failing_tests
			);
		}
		if (body && Array.isArray(body.compile_errors) && body.compile_errors.length > 0) {
			throw new CompileApiError(body.error ?? 'compilation failed', body.compile_errors);
		}
		throw new ApiError(
			res.response.status,
			rawErr as Record<string, unknown> | string | undefined
		);
	}
	return res.data as Template;
}

// ── Template tests ──────────────────────────────────────────────────────────

export async function listTemplateTests(templateId: string): Promise<TemplateTest[]> {
	return unwrap(
		await client.GET('/api/v1/templates/{id}/tests', { params: { path: { id: templateId } } })
	);
}

export async function createTemplateTest(
	templateId: string,
	body: CreateTemplateTestRequest
): Promise<TemplateTest> {
	return unwrap(
		await client.POST('/api/v1/templates/{id}/tests', {
			params: { path: { id: templateId } },
			body
		})
	);
}

export async function updateTemplateTest(
	templateId: string,
	testId: string,
	body: UpdateTemplateTestRequest
): Promise<TemplateTest> {
	return unwrap(
		await client.PATCH('/api/v1/templates/{template_id}/tests/{test_id}', {
			params: { path: { template_id: templateId, test_id: testId } },
			body
		})
	);
}

export async function deleteTemplateTest(templateId: string, testId: string): Promise<void> {
	const res = await client.DELETE('/api/v1/templates/{template_id}/tests/{test_id}', {
		params: { path: { template_id: templateId, test_id: testId } }
	});
	if (res.error !== undefined && res.response.status >= 400) {
		throw new ApiError(
			res.response.status,
			res.error as Record<string, unknown> | string | undefined
		);
	}
}

export async function runTemplateTest(
	templateId: string,
	testId: string
): Promise<TemplateTestRun> {
	return unwrap(
		await client.POST('/api/v1/templates/{template_id}/tests/{test_id}/run', {
			params: { path: { template_id: templateId, test_id: testId } }
		})
	);
}

export async function runAllTemplateTests(
	templateId: string,
	includeDisabled = false
): Promise<RunAllResponse> {
	return unwrap(
		await client.POST('/api/v1/templates/{id}/tests/run-all', {
			params: { path: { id: templateId }, query: { include_disabled: includeDisabled } }
		})
	);
}

export async function listTestRuns(
	templateId: string,
	testId: string,
	limit = 10
): Promise<TemplateTestRun[]> {
	return unwrap(
		await client.GET('/api/v1/templates/{template_id}/tests/{test_id}/runs', {
			params: {
				path: { template_id: templateId, test_id: testId },
				query: { limit }
			}
		})
	);
}

export async function promoteInstanceToTest(
	instanceId: string,
	body: PromoteToTestRequest
): Promise<TemplateTest> {
	return unwrap(
		await client.POST('/api/v1/instances/{id}/promote-to-test', {
			params: { path: { id: instanceId } },
			body
		})
	);
}

export async function createNewVersion(id: string): Promise<Template> {
	return unwrap(
		await client.POST('/api/v1/templates/{id}/new-version', { params: { path: { id } } })
	);
}

export async function getTemplateVersions(id: string): Promise<Template[]> {
	return unwrap(
		await client.GET('/api/v1/templates/{id}/versions', { params: { path: { id } } })
	) as unknown as Template[];
}

export async function compileTemplate(id: string): Promise<object> {
	return unwrap(
		await client.POST('/api/v1/templates/{id}/compile', { params: { path: { id } } })
	) as unknown as object;
}

export async function getTemplateAir(id: string): Promise<object> {
	return unwrap(
		await client.GET('/api/v1/templates/{id}/air', { params: { path: { id } } })
	) as unknown as object;
}

export async function compileGraph(data: CompileRequest): Promise<object> {
	return unwrap(await client.POST('/api/v1/compile', { body: data })) as unknown as object;
}

/** Shape-aware analysis surface — the editor's single source of truth for
 * guard scope + diagnostics. Graph-only: works on drafts that can't compile. */
export type TypeSurfaceResponse = components['schemas']['TypeSurfaceResponse'];

export async function analyzeGraph(data: CompileRequest): Promise<TypeSurfaceResponse> {
	return unwrap(await client.POST('/api/v1/analyze', { body: data })) as TypeSurfaceResponse;
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
		await client.GET('/api/v1/templates/{id}/io-stubs', { params: { path: { id } } })
	) as { generated?: Record<string, Record<string, string>> };
	return res.generated ?? {};
}

/** A leaf field at a Python step's input scope. Picker UIs render this as
 *  the qualified `<slug>.<field>` / `input.<path>` form via `/api/v1/analyze`;
 *  at runtime the Python runner exposes each upstream `<slug>` as a
 *  module global, so the same identifier the picker shows is the exact
 *  identifier the user writes (no `token[...]` wrapping). Kept for the
 *  io-stubs `.pyi` overlay generator. */
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
			await client.GET('/api/v1/templates/{id}/io-stubs', { params: { path: { id } } })
		) as { scopes?: Record<string, StepScopeField[]>; diagnostic?: string };
		return { scopes: res.scopes ?? {}, diagnostic: res.diagnostic ?? 'ok' };
	} catch (e) {
		return { scopes: {}, diagnostic: `request_failed: ${e instanceof Error ? e.message : e}` };
	}
}

// ── Instances ───────────────────────────────────────────────────────────────

export async function listInstances(opts?: {
	page?: number;
	perPage?: number;
	templateId?: string;
	status?: string;
	/// `'live'` (the default), `'draft'`, `'test_run'`, or `'any'` to include
	/// every mode. Omitting hides drafts and test runs.
	mode?: string;
}): Promise<components['schemas']['PaginatedResponse_InstanceListItem']> {
	return unwrap(
		await client.GET('/api/v1/instances', {
			params: {
				query: {
					page: opts?.page ?? 1,
					per_page: opts?.perPage ?? 20,
					template_id: opts?.templateId,
					status: opts?.status,
					mode: opts?.mode
				}
			}
		})
	);
}

export async function getInstance(id: string): Promise<WorkflowInstance> {
	return unwrap(
		await client.GET('/api/v1/instances/{id}', { params: { path: { id } } })
	);
}

export async function createInstance(data: CreateInstanceRequest): Promise<WorkflowInstance> {
	return unwrap(await client.POST('/api/v1/instances', { body: data }));
}

export async function getInstanceState(id: string): Promise<InstanceStateResponse> {
	return unwrap(
		await client.GET('/api/v1/instances/{id}/state', { params: { path: { id } } })
	);
}

/**
 * One row per `(node, iteration)` for an instance — materialized by the
 * step-executions projection consumer. The instance view overlays each
 * row onto its corresponding template node card; clicking opens a drawer
 * with the full inputs/outputs payload.
 */
export async function listStepExecutions(id: string): Promise<StepExecution[]> {
	return unwrap(
		await client.GET('/api/v1/instances/{id}/step-executions', {
			params: { path: { id } }
		})
	) as StepExecution[];
}

export async function cancelInstance(id: string): Promise<void> {
	const res = await client.DELETE('/api/v1/instances/{id}', { params: { path: { id } } });
	if (res.error !== undefined && res.response.status >= 400) {
		throw new ApiError(
			res.response.status,
			res.error as Record<string, unknown> | string | undefined
		);
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
}): Promise<PaginatedProcesses> {
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

/** Processes produced by a given workflow instance (usually one, but a
 *  multi-start template can spawn several). Filters on the TEXT `net_id`
 *  column (`mekhan-{instanceId}`, populated alongside `instance_id`) — the
 *  generic query DSL binds values as text, so filtering the UUID
 *  `instance_id` column directly errors with `uuid = text`. */
export async function listProcessesByInstance(
	instanceId: string
): Promise<PaginatedProcesses> {
	const qs = new URLSearchParams();
	qs.set('filter[net_id][eq]', `mekhan-${instanceId}`);
	qs.set('sort', '-created_at');
	return rawJson(`/processes?${qs.toString()}`);
}

export async function getProcessStats(): Promise<ProcessStats> {
	return unwrap(await client.GET('/api/v1/processes/stats', {}));
}

export async function getProcess(processId: string): Promise<ProcessDetail> {
	return unwrap(
		await client.GET('/api/v1/processes/{process_id}', { params: { path: { process_id: processId } } })
	);
}

export async function updateProcess(
	processId: string,
	data: { name?: string; kind?: string; status?: string; owner?: string }
): Promise<HpiProcess> {
	return unwrap(
		await client.PUT('/api/v1/processes/{process_id}', {
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
		await client.GET('/api/v1/processes/{process_id}/metrics', {
			params: { path: { process_id: processId }, query: params }
		})
	);
}

export async function getProcessMetricsSummary(processId: string): Promise<HpiMetricSummary[]> {
	return unwrap(
		await client.GET('/api/v1/processes/{process_id}/metrics/summary', {
			params: { path: { process_id: processId } }
		})
	);
}

export async function getProcessLogs(
	processId: string,
	params?: { level?: string; source?: string; search?: string; page?: number; page_size?: number }
): Promise<PaginatedLogs> {
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
		await client.GET('/api/v1/processes/{process_id}/tasks', {
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
): Promise<PaginatedArtifacts> {
	const qs = new URLSearchParams();
	if (params?.page !== undefined) qs.set('page', String(params.page));
	if (params?.page_size) qs.set('page_size', String(params.page_size));
	const query = qs.toString();
	return rawJson(`/processes/${processId}/artifacts${query ? `?${query}` : ''}`);
}

// ── Catalogue ───────────────────────────────────────────────────────────────

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
}): Promise<PaginatedArtifacts> {
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
		await client.GET('/api/v1/catalogue/{execution_id}/{id}', {
			params: { path: { execution_id: executionId, id } }
		})
	);
}

export async function getCatalogueStats(): Promise<CatalogueStats> {
	return unwrap(await client.GET('/api/v1/catalogue/stats', {}));
}

export async function getCatalogueStatsByNet(): Promise<CatalogueNetStats[]> {
	return unwrap(await client.GET('/api/v1/catalogue/stats/by-net', {}));
}

export async function getCatalogueLineage(processId: string): Promise<LineageResponse> {
	return unwrap(
		await client.GET('/api/v1/catalogue/lineage/{process_id}', {
			params: { path: { process_id: processId } }
		})
	);
}

export async function getCatalogueDistinct(column: string): Promise<string[]> {
	return unwrap(
		await client.GET('/api/v1/catalogue/distinct/{column}', { params: { path: { column } } })
	);
}

export async function getCatalogueDistinctJsonb(column: string, key: string): Promise<string[]> {
	return unwrap(
		await client.GET('/api/v1/catalogue/distinct-jsonb/{column}/{key}', {
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
		throw new ApiError(res.status, await parseErrorBody(res));
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
		await client.GET('/api/v1/provenance/from-artifact/{execution_id}/{artifact_id}', {
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
		await client.GET('/api/v1/provenance/{net_id}/{token_id}', {
			params: { path: { net_id: netId, token_id: tokenId }, query: { depth } }
		})
	);
}

export async function getEventDetail(netId: string, eventSeq: number): Promise<EventDetail> {
	return unwrap(
		await client.GET('/api/v1/provenance/{net_id}/{event_seq}/detail', {
			params: { path: { net_id: netId, event_seq: eventSeq } }
		})
	);
}

export async function getCrossLink(signalKey: string): Promise<CrossLink> {
	return unwrap(
		await client.GET('/api/v1/provenance/link/{signal_key}', {
			params: { path: { signal_key: signalKey } }
		})
	);
}

// ── Access tokens (embedded PAT management) ────────────────────────────────
//
// Cookie-only by construction on the backend. `listAccessTokens` returns
// `null` (not throws) when the server reports the feature disabled (503, no
// broker configured) so the UI can simply hide the section.

export async function listAccessTokens(): Promise<TokenSummary[] | null> {
	const res = await client.GET('/api/v1/auth/tokens', {});
	if (res.response.status === 503) return null;
	return unwrap(res);
}

export async function createAccessToken(body: CreateTokenRequest): Promise<CreatedToken> {
	return unwrap(await client.POST('/api/v1/auth/tokens', { body }));
}

export async function revokeAccessToken(id: string): Promise<void> {
	const res = await client.DELETE('/api/v1/auth/tokens/{id}', { params: { path: { id } } });
	if (res.error !== undefined && res.response.status >= 400) {
		throw new ApiError(
			res.response.status,
			res.error as Record<string, unknown> | string | undefined
		);
	}
}

// ── Workspaces / projects / tags / visibility / me / users ─────────────────

export async function listWorkspaces(): Promise<WorkspaceSummary[]> {
	return unwrap(await client.GET('/api/v1/workspaces', {}));
}

export async function getWorkspace(id: string): Promise<WorkspaceSummary> {
	return unwrap(
		await client.GET('/api/v1/workspaces/{id}', { params: { path: { id } } })
	);
}

export async function listWorkspaceMembers(id: string): Promise<WorkspaceMember[]> {
	return unwrap(
		await client.GET('/api/v1/workspaces/{id}/members', {
			params: { path: { id } }
		})
	);
}

export async function addWorkspaceMember(
	id: string,
	body: AddMemberRequest
): Promise<WorkspaceMember> {
	return unwrap(
		await client.POST('/api/v1/workspaces/{id}/members', {
			params: { path: { id } },
			body
		})
	);
}

export async function removeWorkspaceMember(id: string, userId: string): Promise<void> {
	const res = await client.DELETE('/api/v1/workspaces/{id}/members/{user_id}', {
		params: { path: { id, user_id: userId } }
	});
	if (res.response.ok) return;
	throw new ApiError(res.response.status, res.error as Record<string, unknown> | string | undefined);
}

export async function listWorkspaceTags(workspaceId: string): Promise<string[]> {
	return unwrap(
		await client.GET('/api/v1/workspaces/{id}/tags', {
			params: { path: { id: workspaceId } }
		})
	);
}

export async function listProjects(workspaceId: string): Promise<Project[]> {
	return unwrap(
		await client.GET('/api/v1/workspaces/{id}/projects', {
			params: { path: { id: workspaceId } }
		})
	);
}

export async function createProject(
	workspaceId: string,
	body: CreateProjectRequest
): Promise<Project> {
	return unwrap(
		await client.POST('/api/v1/workspaces/{id}/projects', {
			params: { path: { id: workspaceId } },
			body
		})
	);
}

export async function deleteProject(projectId: string): Promise<void> {
	const res = await client.DELETE('/api/v1/projects/{id}', {
		params: { path: { id: projectId } }
	});
	if (res.response.ok) return;
	throw new ApiError(res.response.status, res.error as Record<string, unknown> | string | undefined);
}

export async function attachTemplateToProject(
	projectId: string,
	templateId: string
): Promise<void> {
	const res = await client.POST('/api/v1/projects/{id}/templates', {
		params: { path: { id: projectId } },
		body: { template_id: templateId }
	});
	if (res.response.ok) return;
	throw new ApiError(res.response.status, res.error as Record<string, unknown> | string | undefined);
}

export async function detachTemplateFromProject(
	projectId: string,
	baseTemplateId: string
): Promise<void> {
	const res = await client.DELETE(
		'/api/v1/projects/{id}/templates/{base_template_id}',
		{ params: { path: { id: projectId, base_template_id: baseTemplateId } } }
	);
	if (res.response.ok) return;
	throw new ApiError(res.response.status, res.error as Record<string, unknown> | string | undefined);
}

export async function getTemplateTags(templateId: string): Promise<string[]> {
	return unwrap(
		await client.GET('/api/v1/templates/{id}/tags', {
			params: { path: { id: templateId } }
		})
	);
}

export async function setTemplateTags(templateId: string, tags: string[]): Promise<string[]> {
	return unwrap(
		await client.PUT('/api/v1/templates/{id}/tags', {
			params: { path: { id: templateId } },
			body: { tags }
		})
	);
}

export async function setTemplateVisibility(
	templateId: string,
	visibility: 'workspace' | 'public'
): Promise<void> {
	const res = await client.PATCH('/api/v1/templates/{id}/visibility', {
		params: { path: { id: templateId } },
		body: { visibility }
	});
	if (res.response.ok) return;
	throw new ApiError(res.response.status, res.error as Record<string, unknown> | string | undefined);
}

/// POST /api/v1/me/active-workspace — sets the active-workspace cookie.
/// The picker calls `auth.refresh()` after this resolves so the in-memory
/// session reflects the new workspace_id on the very next page paint.
export async function setActiveWorkspace(workspaceId: string): Promise<void> {
	const res = await client.POST('/api/v1/me/active-workspace', {
		body: { workspace_id: workspaceId }
	});
	if (res.response.ok) return;
	throw new ApiError(res.response.status, res.error as Record<string, unknown> | string | undefined);
}

export async function clearActiveWorkspace(): Promise<void> {
	const res = await client.DELETE('/api/v1/me/active-workspace', {});
	if (res.response.ok) return;
	throw new ApiError(res.response.status, res.error as Record<string, unknown> | string | undefined);
}

export async function resolveUserByEmail(email: string): Promise<ResolveEmailResponse> {
	return unwrap(await client.POST('/api/v1/users/resolve', { body: { email } }));
}

/// GET /api/v1/workspaces/{ws}/projects/{p}/openapi.json — synthesized
/// webhook spec for the project. Body is a raw OpenAPI 3.0.3 document
/// (free-form JSON), surfaced via `rawJson` so we don't fight openapi-fetch
/// over a hand-built schema.
export async function getProjectOpenApiBundle(
	workspaceId: string,
	projectId: string
): Promise<Record<string, unknown>> {
	return rawJson(`/workspaces/${workspaceId}/projects/${projectId}/openapi.json`);
}

// ── Raw-JSON helper for query-DSL endpoints ────────────────────────────────
//
// Most JSON endpoints route through `client.GET/POST/...` via openapi-fetch
// and pick up types from `schema.d.ts`. A subset cannot:
//
//   1. The paginated query-DSL routes (`/processes`, `/processes/{}/logs`,
//      `/processes/{}/artifacts`, `/catalogue`, `/tasks`) accept bracket
//      notation in the query string (`filter[col][op]=value&sort=-col&...`)
//      that utoipa does not derive `IntoParams` for. The RESPONSE shape IS
//      typed in the spec — call sites cast through `Paginated_*` schema
//      aliases — but the REQUEST query has to be hand-built. Adding
//      `IntoParams` declarations on the Rust handlers would let these flow
//      through `client.GET`; tracked as a follow-up.
//   2. The HumanTask-shaped endpoints (`/tasks/{id}`, `/tasks/{id}/complete`,
//      `/tasks/{id}/cancel`) return a heterogeneous JSON object assembled
//      from `HpiTask.detail` — the wire shape is intentionally `Value`.
//
// `rawJson` is exported so call sites outside this module share the same
// `ApiError` envelope decoding. Path is API_BASE-relative (e.g.
// `'/tasks/{id}'`).
export async function rawJson<T>(path: string, init?: RequestInit): Promise<T> {
	const res = await authFetch(`${API_BASE}${path}`, {
		...init,
		headers: { 'Content-Type': 'application/json', ...init?.headers }
	});
	if (!res.ok) {
		throw new ApiError(res.status, await parseErrorBody(res));
	}
	return res.json() as Promise<T>;
}

/// Try to decode an error `Response` as the standard `ErrorResponse` JSON
/// envelope; fall back to the raw body text. Used by every direct-fetch call
/// site (multipart uploads, SSE) so they still surface the structured `code`.
async function parseErrorBody(res: Response): Promise<Record<string, unknown> | string> {
	const text = await res.text();
	try {
		const parsed = JSON.parse(text);
		return typeof parsed === 'object' && parsed !== null ? parsed : text;
	} catch {
		return text;
	}
}
