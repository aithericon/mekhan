/**
 * Platform-tier helpers for the Resource UI (Phase 4).
 *
 * The platform tier is a global resource scope that sits ABOVE any single
 * workspace. Backend contract:
 *   - `ResourceDetail.scope_kind` may be `'platform'` (the precise signal).
 *   - `ResourceSummary` does NOT expose `scope_kind` (see schema.d.ts) — it
 *     only carries `my_effective_role`. A platform resource is globally
 *     visible and stamps `my_effective_role = 'viewer'` for everyone except
 *     a platform admin (who gets `'owner'`). So on a list row the read-only
 *     `viewer` role is the fallback signal the UI keys off.
 *   - Resource create accepts `scope_kind: 'platform'`; the backend 403s the
 *     POST unless the caller `is_platform_admin`.
 *
 * These helpers centralize the detection so the badge, the read-only gating,
 * and any future surface can't drift apart.
 */

/** A row/detail shape that carries the fields these helpers read. Both
 *  `ResourceSummary` and `ResourceDetail` structurally satisfy this. */
export interface ResourceTierShape {
	scope_kind?: string | null;
	my_effective_role?: string | null;
}

/**
 * Whether a resource belongs to the global platform tier.
 *
 * Prefers the precise `scope_kind === 'platform'` when present (detail view).
 * On a list summary — which has no `scope_kind` — falls back to the
 * documented signal: a globally-visible row the caller can only *view*
 * (`my_effective_role === 'viewer'`). The fallback is only consulted when
 * `scope_kind` is absent, so the detail view stays exact.
 */
export function isPlatformResource(r: ResourceTierShape): boolean {
	if (r.scope_kind != null) return r.scope_kind === 'platform';
	return r.my_effective_role === 'viewer';
}

/**
 * Whether the current caller may mutate this resource (edit / delete / rotate /
 * move). Read-only for anyone whose effective role is `viewer`, which folds in
 * the platform tier: a non-admin's view of a platform resource is `viewer`, and
 * a platform admin's is `owner`. View / run affordances are never gated by this.
 */
export function canMutateResource(r: ResourceTierShape): boolean {
	return r.my_effective_role != null && r.my_effective_role !== 'viewer';
}
