// Backend → create-kind resolution for NewCapacityModal's EDIT mode.
//
// A capacity's dispatch `backend` mostly names the create kind that authored it
// (presence→runner_group, tokens→limit, queue→worker, scheduler→cluster). The one
// ambiguity: **human pools are ALSO `presence`** — they differ from runner groups
// only on the acceptance axis (`consent` vs `auto`). So when editing a `presence`
// capacity we have to peek at the axes to tell the two apart.

import type { CapacityBackend, CapacitySummary } from '$lib/api/capacities';

export type Kind = 'runner_group' | 'limit' | 'worker' | 'cluster' | 'human';

// The dispatch backend → which create kind authored it. A `null`/absent backend
// (a not-yet-dispatchable capacity) has no dedicated kind; it edits name-only
// under the worker branch (its config/axes are never re-sent).
const BACKEND_KIND: Record<CapacityBackend, Kind> = {
	presence: 'runner_group',
	tokens: 'limit',
	queue: 'worker',
	scheduler: 'cluster'
};

/**
 * Resolve the locked create-kind for an existing capacity being edited.
 *
 * `presence` + `acceptance: 'consent'` ⇒ `'human'` (a member-consent people
 * pool); every other `presence` (auto/absent axes) ⇒ `'runner_group'`. All
 * other backends map straight through `BACKEND_KIND`; a `null`/absent backend
 * falls back to `'worker'` (name-only edit).
 */
export function resolveEditKind(target: Pick<CapacitySummary, 'backend' | 'axes'>): Kind {
	if (target.backend === 'presence' && target.axes?.acceptance === 'consent') {
		return 'human';
	}
	return target.backend == null ? 'worker' : BACKEND_KIND[target.backend];
}
