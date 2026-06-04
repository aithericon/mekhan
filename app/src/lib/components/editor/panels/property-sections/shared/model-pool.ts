import type { ModelSetView } from '$lib/api/models';

/**
 * The internal-pool model picker's filter rule: an internal binding may ONLY
 * select a model the control plane reports as `available` — the AND-gate
 * (`state == loaded` AND a LIVE runner advertises it). A merely-`loaded`
 * model whose runners are all dead is NOT offerable. Pure + colocated in a
 * `.ts` module so the filter is unit-tested without a DOM (matches the
 * `schema/model.ts` test convention).
 */
export function availableModelIds(models: ModelSetView[]): string[] {
	return models.filter((m) => m.available).map((m) => m.model_id);
}
