// Fleet grouping — the one place the "split the fleet into its groups" logic
// lives, shared by the Runners list and the Live board so they can't drift.
//
// A runner's `group` is an alias string. It is only meaningful when BACKED by a
// presence `capacity` *resource* (the thing that carries the presence-pool net
// the runner's unit is admitted into). This helper joins the three live inputs —
// the runners, their presence snapshot, and the presence `capacity` resources —
// into ordered sections an operator can read at a glance:
//
//   1. backed     — one per presence `capacity` resource (shown even with 0 members,
//                    so a created-but-empty group is visible), sorted by alias.
//   2. model       — runners that carry NO group but serve model engines. These
//                    are the self-hosted model pool / LLM engines: ungrouped BY
//                    DESIGN (inference goes over HTTP, not presence dispatch), so
//                    they're a first-class role, NOT uncategorized leftovers.
//                    Split out so the model pool reads as itself, not "ungrouped".
//   3. unbacked    — a group alias some runner carries that resolves to NO
//                    resource → NO pool net → those runners heartbeat but are
//                    admitted to nothing. Surfaced loudly; should be unreachable
//                    via the UI now that token-mint requires a backing group,
//                    but legacy rows (or a deleted group resource) land here.
//   4. ungrouped   — runners with no group AND no model role: the genuine
//                    leftovers (last).
//
// Backend-coverage + online counts are computed from the live presence snapshot
// (only a PRESENT runner advertises backends / counts as online).
import type { RunnerSummary, RunnerPresenceSnapshot } from '$lib/api/runners';
import type { ResourceSummary } from '$lib/api/resources';

export type FleetSectionKind = 'backed' | 'model' | 'unbacked' | 'ungrouped';

export interface FleetSection {
	kind: FleetSectionKind;
	/** Group alias (the `runner_group` resource path); `null` for the ungrouped bucket. */
	alias: string | null;
	/** The backing presence `capacity` resource — present only for `kind === 'backed'`. */
	resource: ResourceSummary | null;
	/** Runners in this section. */
	runners: RunnerSummary[];
	/** How many of this section's runners are currently present (online). */
	onlineCount: number;
	/** Union of executor backends advertised by this section's PRESENT runners. */
	backends: string[];
}

type PresenceById = Record<string, RunnerPresenceSnapshot | undefined>;

/**
 * Group the fleet into ordered sections. Pure — no Svelte, no fetching.
 *
 * @param runners        all enrolled runners (each may carry a `group` alias)
 * @param presenceById   runner_id → live presence snapshot (for online + backends)
 * @param groupResources the presence `capacity` resources (the backed groups)
 */
/**
 * Constrain a runner list + its backing group resources to a single group alias.
 *
 * Used by the per-group detail views (RunnerList / InterfacesCatalog given a
 * `group` prop): keeps only the runners whose `group` matches `alias` and
 * only the backing resource whose `path` matches. A `null`/`undefined` alias is the
 * no-op (the full cross-group lists pass through unchanged) — so the default,
 * unfiltered behaviour of every existing caller is preserved.
 */
export function filterFleetByGroup<R extends { group?: string | null }>(
	runners: R[],
	groupResources: ResourceSummary[],
	alias: string | null | undefined
): { runners: R[]; groupResources: ResourceSummary[] } {
	if (alias == null) return { runners, groupResources };
	return {
		runners: runners.filter((r) => (r.group ?? null) === alias),
		groupResources: groupResources.filter((g) => g.path === alias)
	};
}

/**
 * @param modelServerIds runner ids that serve model engines (the Engines facet).
 *        An UNGROUPED runner in this set is surfaced as a first-class "Model
 *        servers" section rather than dumped in "ungrouped". A grouped runner
 *        stays in its (backed/unbacked) group — there it IS a dispatch target.
 *        Defaults to empty (callers that don't know the engine facet, e.g. the
 *        Live board, keep the plain backed/unbacked/ungrouped split).
 */
export function groupFleet(
	runners: RunnerSummary[],
	presenceById: PresenceById,
	groupResources: ResourceSummary[],
	modelServerIds: Set<string> = new Set()
): FleetSection[] {
	// alias → backing resource (a presence `capacity` resource's `path` is its alias).
	const resourceByAlias = new Map<string, ResourceSummary>();
	for (const r of groupResources) resourceByAlias.set(r.path, r);

	// Bucket runners by their group alias (null → ungrouped).
	const byAlias = new Map<string | null, RunnerSummary[]>();
	for (const runner of runners) {
		const key = runner.group ?? null;
		const bucket = byAlias.get(key);
		if (bucket) bucket.push(runner);
		else byAlias.set(key, [runner]);
	}

	const onlineOf = (rs: RunnerSummary[]) =>
		rs.filter((r) => presenceById[r.id]?.present).length;
	const backendsOf = (rs: RunnerSummary[]) => {
		const set = new Set<string>();
		for (const r of rs) {
			const snap = presenceById[r.id];
			if (!snap?.present) continue;
			for (const be of snap.backends ?? []) set.add(be);
		}
		return [...set].sort();
	};

	const backed: FleetSection[] = [];
	const unbacked: FleetSection[] = [];

	// Backed: one per resource (sorted by alias), even when it has no members.
	const backedAliases = [...resourceByAlias.keys()].sort();
	for (const alias of backedAliases) {
		const rs = byAlias.get(alias) ?? [];
		backed.push({
			kind: 'backed',
			alias,
			resource: resourceByAlias.get(alias) ?? null,
			runners: rs,
			onlineCount: onlineOf(rs),
			backends: backendsOf(rs)
		});
	}

	// Unbacked: a runner's alias that has no backing resource. Sorted by alias.
	const unbackedAliases = [...byAlias.keys()]
		.filter((k): k is string => k !== null && !resourceByAlias.has(k))
		.sort();
	for (const alias of unbackedAliases) {
		const rs = byAlias.get(alias) ?? [];
		unbacked.push({
			kind: 'unbacked',
			alias,
			resource: null,
			runners: rs,
			onlineCount: onlineOf(rs),
			backends: backendsOf(rs)
		});
	}

	// The group-less runners split two ways: model servers (a first-class role —
	// ungrouped by design because inference is HTTP, not presence-dispatched) and
	// the genuine leftovers.
	const groupless = byAlias.get(null) ?? [];
	const modelRunners = groupless.filter((r) => modelServerIds.has(r.id));
	const ungroupedRunners = groupless.filter((r) => !modelServerIds.has(r.id));

	const model: FleetSection[] =
		modelRunners.length > 0
			? [
					{
						kind: 'model',
						alias: null,
						resource: null,
						runners: modelRunners,
						onlineCount: onlineOf(modelRunners),
						backends: backendsOf(modelRunners)
					}
				]
			: [];

	const ungrouped: FleetSection[] =
		ungroupedRunners.length > 0
			? [
					{
						kind: 'ungrouped',
						alias: null,
						resource: null,
						runners: ungroupedRunners,
						onlineCount: onlineOf(ungroupedRunners),
						backends: backendsOf(ungroupedRunners)
					}
				]
			: [];

	// Order: real groups, then the model pool, then anomalies, then leftovers.
	return [...backed, ...model, ...unbacked, ...ungrouped];
}
