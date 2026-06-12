/**
 * Pure helpers for the editor toolbar's Runs menu (EditorRunsMenu.svelte) —
 * extracted so the deep-link shape and row labels are unit-testable without
 * mounting the dropdown.
 */
import { timeAgo } from '$lib/utils';
import type { InstanceListItem, Template } from '$lib/api/client';

/** How many recent runs the toolbar menu shows before "View all". */
export const RUNS_MENU_LIMIT = 8;

/**
 * A template's version-chain family id: the chain root when this row is a
 * forked version, else its own id (`COALESCE(base_template_id, id)` — same
 * rule as the backend's `chain_root_id`).
 */
export function templateFamilyId(t: Pick<Template, 'id' | 'base_template_id'>): string {
	return t.base_template_id ?? t.id;
}

/**
 * Instances-list deep-link scoped to the whole template family. `mode=any` on
 * purpose: the menu surfaces draft/test runs too, so "View all" must not
 * silently drop them behind the list's live-only default.
 */
export function allRunsHref(familyId: string): string {
	return `/instances?template_family=${encodeURIComponent(familyId)}&mode=any`;
}

/**
 * Relative "when" line for a run row. Prefers the actual start time; an
 * instance that never started (status `created`, or rejected before launch)
 * falls back to its creation time — labeled accordingly.
 */
export function runWhenLabel(
	run: Pick<InstanceListItem, 'started_at' | 'created_at'>,
	now?: Date
): string {
	return run.started_at
		? `started ${timeAgo(run.started_at, now)}`
		: `created ${timeAgo(run.created_at, now)}`;
}
