/**
 * Shared template family-id utility.
 *
 * `listTemplates(published=true)` returns the latest published row per
 * family. The stable identifier to persist is `base_template_id ?? id` —
 * the root row of the version chain. Both ChildWorkflowBrowser and
 * SubWorkflowSection need this derivation; keeping it in one place prevents
 * the two from drifting.
 */
import type { Template } from '$lib/api/client';

/**
 * Return the stable family id for a template row.
 * A v1 row has no `base_template_id` (it IS the family root), so we fall
 * back to `id`. Every subsequent version carries `base_template_id = v1.id`.
 */
export function familyId(t: Pick<Template, 'id' | 'base_template_id'>): string {
	return t.base_template_id ?? t.id;
}
