// Backend → create-kind resolution for NewCapacityModal's EDIT mode.
//
// A capacity's dispatch `backend` mostly names the create kind that authored it
// (presence→runner_group, tokens→limit, queue→worker, scheduler→cluster). The one
// ambiguity: **human pools are ALSO `presence`** — they differ from runner groups
// only on the dispatch axis (`offer` vs `pull`). So when editing a `presence`
// capacity we have to peek at the axes to tell the two apart.

import type { CapacityBackend, CapacitySummary } from '$lib/api/capacities';

export type Kind = 'runner_group' | 'limit' | 'worker' | 'cluster' | 'human';

// The dispatch backend → which create kind authored it. `deferred` (the
// not-yet-dispatchable `consume` path) has no dedicated kind; it edits
// name-only under the worker branch (its config/axes are never re-sent).
const BACKEND_KIND: Record<CapacityBackend, Kind> = {
	presence: 'runner_group',
	tokens: 'limit',
	queue: 'worker',
	scheduler: 'cluster',
	deferred: 'worker'
};

/**
 * Resolve the locked create-kind for an existing capacity being edited.
 *
 * `presence` + `dispatch: 'offer'` ⇒ `'human'` (a self-claiming people pool);
 * every other `presence` (pull/push/none/absent axes) ⇒ `'runner_group'`. All
 * other backends map straight through `BACKEND_KIND`.
 */
export function resolveEditKind(target: Pick<CapacitySummary, 'backend' | 'axes'>): Kind {
	if (target.backend === 'presence' && target.axes?.dispatch === 'offer') {
		return 'human';
	}
	return BACKEND_KIND[target.backend];
}
