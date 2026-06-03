// The "Run on" target resolution for an AutomatedStep's deployment model —
// extracted as pure functions so the (regression-prone) ambiguity handling is
// unit-testable without mounting the Select.
//
// An AutomatedStep's `deploymentModel` is `executor{capacity?}` | `scheduled{…}`.
// The editor presents four first-class TARGETS:
//   workers      — executor, no capacity (fungible worker pool / inline effect)
//   runner_group — executor, capacity bound to a `runner_group` resource
//   limit        — executor, capacity bound to a `concurrency_limit` resource
//   scheduled    — scheduled cluster
//
// runner_group vs limit can only be told apart by resolving the bound alias's
// resource KIND. So a capacity binding whose alias isn't chosen YET is ambiguous
// — `resolveRunTarget` returns `null` for it, signalling the caller to keep the
// user's local choice rather than snapping back to `workers` (the bug this guards).

export type RunTarget = 'workers' | 'runner_group' | 'limit' | 'scheduled';

export type DeploymentLike =
	| { mode: 'executor'; capacity?: null | { alias?: string } }
	| { mode: 'scheduled'; [k: string]: unknown };

/**
 * Resolve the target from the persisted value + the resolved kind of any bound
 * alias. Returns `null` when the value is AMBIGUOUS (an executor capacity with no
 * alias chosen yet) — the caller must then preserve its local target choice.
 */
export function resolveRunTarget(
	value: DeploymentLike | undefined,
	kindByAlias: Map<string, 'runner_group' | 'concurrency_limit'>
): RunTarget | null {
	if (value?.mode === 'scheduled') return 'scheduled';
	if (value?.mode !== 'executor') return 'workers';
	const cap = value.capacity;
	if (cap == null) return 'workers';
	const alias = cap.alias ?? '';
	if (!alias) return null; // capacity present, alias not chosen yet → ambiguous
	return kindByAlias.get(alias) === 'runner_group' ? 'runner_group' : 'limit';
}

/**
 * The initial target to seed local state with, WITHOUT needing the resource
 * lists (which load async). A bound step starts as `limit` provisionally; the
 * caller's effect refines it to `runner_group` once the alias's kind resolves.
 */
export function initialRunTarget(value: DeploymentLike | undefined): RunTarget {
	if (value?.mode === 'scheduled') return 'scheduled';
	if (value?.mode === 'executor' && value.capacity != null) return 'limit';
	return 'workers';
}
