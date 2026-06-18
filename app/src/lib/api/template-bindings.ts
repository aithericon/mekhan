/**
 * Typed wrappers for the run-time resource/pool BINDING surface (Phase E).
 *
 * A published template auto-derives a requirements manifest — one
 * {@link RequirementSlot} per distinct resource/pool reference — at compile
 * time. Those slots are bound at INSTANCE-CREATION time through a precedence
 * chain: per-instance override → per-workspace default → platform auto-bind →
 * home-workspace baseline. These helpers cover the two endpoints the binding UI
 * reads/writes:
 *   - `GET  /api/v1/templates/{id}/requirements` — the manifest + per-current-
 *     workspace readiness (which slots resolve, by which tier).
 *   - `PUT  /api/v1/templates/{id}/bindings` — upsert the calling workspace's
 *     DEFAULT bindings for one or more slots.
 *
 * Kept in a sibling file (not folded into the 1900-line `client.ts`) for the
 * same reason `resources.ts` is independent — the binding surface is a
 * self-contained chunk and both files share the generated `paths` / `components`
 * types so they stay in lockstep automatically.
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

export type RequirementSlot = components['schemas']['RequirementSlot'];
export type SlotRole = components['schemas']['SlotRole'];
export type SlotReadiness = components['schemas']['SlotReadiness'];
export type BindingTier = components['schemas']['BindingTier'];
export type SlotBindingInput = components['schemas']['SlotBindingInput'];
export type PutBindingsRequest = components['schemas']['PutBindingsRequest'];
export type TemplateRequirementsResponse =
	components['schemas']['TemplateRequirementsResponse'];

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

/**
 * GET /api/v1/templates/{id}/requirements — the template's auto-derived
 * resource/pool requirement manifest plus per-CURRENT-workspace readiness: for
 * each slot, whether it is satisfied and by which binding tier. A template with
 * no resource/pool refs returns an empty `slots`/`readiness` (and
 * `launchable: true`).
 */
export async function getTemplateRequirements(
	id: string
): Promise<TemplateRequirementsResponse> {
	return unwrap(
		await client.GET('/api/v1/templates/{id}/requirements', { params: { path: { id } } })
	);
}

/**
 * PUT /api/v1/templates/{id}/bindings — upsert the calling workspace's DEFAULT
 * bindings for one or more requirement slots. Returns the refreshed readiness so
 * the caller sees the effect immediately. Requires Editor on the template.
 */
export async function putTemplateBindings(
	id: string,
	bindings: SlotBindingInput[]
): Promise<TemplateRequirementsResponse> {
	return unwrap(
		await client.PUT('/api/v1/templates/{id}/bindings', {
			params: { path: { id } },
			body: { bindings }
		})
	);
}

/** Human-readable label for a binding tier (the picker shows it next to a
 *  satisfied slot so the user knows where the binding comes from). */
export function bindingTierLabel(tier: BindingTier | null | undefined): string {
	switch (tier) {
		case 'instance_override':
			return 'Per-run override';
		case 'workspace_default':
			return 'Workspace default';
		case 'platform_auto_bind':
			return 'Platform resource';
		case 'home_baseline':
			return 'Template baseline';
		default:
			return '';
	}
}

/** The `resource_type` a slot expects — used to filter the resource picker so
 *  only type-compatible resources are offered (the backend enforces the same
 *  match at launch / bind time). */
export function slotResourceType(slot: RequirementSlot): string {
	return slot.resource_type;
}
