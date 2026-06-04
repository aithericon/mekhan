// The "Run on" target resolution for an AutomatedStep's deployment model —
// extracted as pure functions so the (regression-prone) ambiguity handling is
// unit-testable without mounting the Select.
//
// An AutomatedStep's `deploymentModel` is `executor{capacity?}` | `scheduled{…}`.
// The editor presents four first-class TARGETS:
//   workers      — executor, no capacity (fungible worker pool / inline effect)
//   runner_group — executor, capacity bound to a presence `capacity` resource
//   limit        — executor, capacity bound to a seeded `capacity` resource
//   scheduled    — scheduled cluster
//
// runner_group vs limit are both `capacity` resources now — they differ only in
// the `liveness` axis carried in the resource's `public_config` (presence vs
// seeded). The single `capacity` kind is discriminated by that axis. A capacity
// binding whose alias isn't chosen YET is ambiguous — `resolveRunTarget` returns
// `null` for it, signalling the caller to keep the user's local choice rather
// than snapping back to `workers` (the bug this guards).

import type { ResourceSummary } from '$lib/api/resources';

export type RunTarget = 'workers' | 'runner_group' | 'limit' | 'scheduled';

export type DeploymentLike =
	| { mode: 'executor'; capacity?: null | { alias?: string } }
	| { mode: 'scheduled'; [k: string]: unknown };

/**
 * The "Run on" target a `capacity` resource maps to, derived from its
 * `liveness` axis (stored in `public_config`):
 *   presence           → runner_group  (a presence/instrument pool)
 *   seeded             → limit         (a seeded concurrency limit)
 *   competing_consumer → 'workers'     (a fungible worker pool — no held claim)
 * Anything else (including a missing axis) falls back to `limit` so a capacity
 * with config we can't read is still bindable rather than vanishing.
 */
export function capacityTarget(resource: ResourceSummary): RunTarget {
	const liveness = (resource.public_config as { liveness?: string } | null | undefined)?.liveness;
	if (liveness === 'presence') return 'runner_group';
	if (liveness === 'competing_consumer') return 'workers';
	return 'limit';
}

/**
 * Build the alias → run-target map the deployment picker discriminates on, from
 * the loaded `capacity` resources. The picker's runner_group/limit branches each
 * filter the same list by this map.
 */
export function targetsByAlias(capacities: ResourceSummary[]): Map<string, RunTarget> {
	const m = new Map<string, RunTarget>();
	for (const r of capacities) m.set(r.path, capacityTarget(r));
	return m;
}

/**
 * Resolve the target from the persisted value + the resolved target of any bound
 * alias. Returns `null` when the value is AMBIGUOUS (an executor capacity with no
 * alias chosen yet) — the caller must then preserve its local target choice.
 */
export function resolveRunTarget(
	value: DeploymentLike | undefined,
	targetByAlias: Map<string, RunTarget>
): RunTarget | null {
	if (value?.mode === 'scheduled') return 'scheduled';
	if (value?.mode !== 'executor') return 'workers';
	const cap = value.capacity;
	if (cap == null) return 'workers';
	const alias = cap.alias ?? '';
	if (!alias) return null; // capacity present, alias not chosen yet → ambiguous
	// Unknown alias (resources not loaded / deleted) → provisional `limit`.
	return targetByAlias.get(alias) ?? 'limit';
}

/**
 * The initial target to seed local state with, WITHOUT needing the resource
 * lists (which load async). A bound step starts as `limit` provisionally; the
 * caller's effect refines it to `runner_group` once the alias's axis resolves.
 */
export function initialRunTarget(value: DeploymentLike | undefined): RunTarget {
	if (value?.mode === 'scheduled') return 'scheduled';
	if (value?.mode === 'executor' && value.capacity != null) return 'limit';
	return 'workers';
}
