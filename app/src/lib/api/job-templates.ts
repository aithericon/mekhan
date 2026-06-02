/**
 * Typed wrappers for the Phase 3 B-model job-template CRUD endpoints.
 *
 * Mirrors `$lib/api/resources.ts`: same `openapi-fetch` client pattern,
 * the "throws on non-2xx" contract, and `components['schemas'][...]` aliases
 * so call sites never reach into `schema.d.ts` directly.
 */
import createClient, { type Middleware } from 'openapi-fetch';
import type { components, paths } from './schema';

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

const client = createClient<paths>({ baseUrl: '', credentials: 'same-origin' });
client.use(sessionExpiryMiddleware);

// ── Type aliases ───────────────────────────────────────────────────────────

export type JobTemplateSummary = components['schemas']['JobTemplateSummary'];
export type JobTemplateDetail = components['schemas']['JobTemplateDetail'];
export type JobTemplateVersion = components['schemas']['JobTemplateVersion'];
export type CommonSpec = components['schemas']['CommonSpec'];
export type EscapeHatch = components['schemas']['EscapeHatch'];
export type TemplateParameter = components['schemas']['TemplateParameter'];
export type TemplateStaging = components['schemas']['TemplateStaging'];
export type CreateJobTemplateRequest = components['schemas']['CreateJobTemplateRequest'];
export type UpdateJobTemplateRequest = components['schemas']['UpdateJobTemplateRequest'];
export type StageJobTemplateRequest = components['schemas']['StageJobTemplateRequest'];
export type PaginatedJobTemplates = components['schemas']['PaginatedResponse_JobTemplateSummary'];

// ── Helpers ────────────────────────────────────────────────────────────────

function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
	if (result.error !== undefined) {
		const status = result.response.status;
		const body =
			typeof result.error === 'object' ? JSON.stringify(result.error) : String(result.error);
		throw new Error(`API error ${status}: ${body}`);
	}
	if (result.data === undefined) {
		throw new Error(`API error ${result.response.status}: empty body`);
	}
	return result.data;
}

// ── Endpoints ──────────────────────────────────────────────────────────────

export interface ListJobTemplatesParams {
	page?: number;
	perPage?: number;
	/** Filter to a specific scheduler flavor: `slurm` | `nomad`. */
	flavor?: string;
	workspace_id?: string;
}

export async function listJobTemplates(
	params?: ListJobTemplatesParams
): Promise<PaginatedJobTemplates> {
	return unwrap(
		await client.GET('/api/v1/job-templates', {
			params: {
				query: {
					page: params?.page ?? 1,
					per_page: params?.perPage ?? 100,
					flavor: params?.flavor,
					workspace_id: params?.workspace_id
				}
			}
		})
	);
}

export async function getJobTemplate(id: string): Promise<JobTemplateDetail> {
	return unwrap(
		await client.GET('/api/v1/job-templates/{id}', { params: { path: { id } } })
	);
}

export async function createJobTemplate(
	body: CreateJobTemplateRequest
): Promise<JobTemplateSummary> {
	return unwrap(await client.POST('/api/v1/job-templates', { body }));
}

export async function updateJobTemplate(
	id: string,
	body: UpdateJobTemplateRequest
): Promise<JobTemplateSummary> {
	return unwrap(
		await client.PUT('/api/v1/job-templates/{id}', { params: { path: { id } }, body })
	);
}

export async function deleteJobTemplate(id: string): Promise<void> {
	const res = await client.DELETE('/api/v1/job-templates/{id}', {
		params: { path: { id } }
	});
	if (res.response.status >= 400) {
		const detail = res.error ? JSON.stringify(res.error) : '';
		throw new Error(`API error ${res.response.status}: ${detail}`);
	}
}

export async function listJobTemplateStagings(id: string): Promise<TemplateStaging[]> {
	return unwrap(
		await client.GET('/api/v1/job-templates/{id}/stagings', { params: { path: { id } } })
	);
}

/**
 * POST /api/v1/job-templates/{id}/stage — trigger staging of a template version
 * onto one or more datacenters. Returns 202 with the created/updated staging rows.
 *
 * Pass `datacenter_resource_ids: [thisClusterResourceId]` to stage only to the
 * cluster the user is viewing. Omit to stage to every compatible datacenter in
 * the workspace.
 */
export async function stageJobTemplate(
	id: string,
	body: StageJobTemplateRequest
): Promise<TemplateStaging[]> {
	return unwrap(
		await client.POST('/api/v1/job-templates/{id}/stage', {
			params: { path: { id } },
			body
		})
	);
}
