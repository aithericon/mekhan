import { type ClassValue, clsx } from 'clsx';
import { twMerge } from 'tailwind-merge';

export function cn(...inputs: ClassValue[]) {
	return twMerge(clsx(inputs));
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type WithoutChild<T> = T extends { child?: any } ? Omit<T, 'child'> : T;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type WithoutChildren<T> = T extends { children?: any } ? Omit<T, 'children'> : T;
export type WithoutChildrenOrChild<T> = WithoutChildren<WithoutChild<T>>;
export type WithElementRef<T, U extends HTMLElement = HTMLElement> = T & { ref?: U | null };

/**
 * Compact relative time ("just now", "3m ago", "2h ago", "5d ago", "3w ago",
 * "4mo ago", "2y ago"). Used by AuthorshipChips (Phase 5) for the
 * created/updated lines. Falls back to the localized date string for anything
 * older than ~a year, and tolerates a null/invalid input by returning ''.
 *
 * `now` is injectable so tests are deterministic without mocking the clock.
 */
export function timeAgo(input: string | Date | null | undefined, now: Date = new Date()): string {
	if (!input) return '';
	const then = input instanceof Date ? input : new Date(input);
	const ms = then.getTime();
	if (Number.isNaN(ms)) return '';
	const secs = Math.round((now.getTime() - ms) / 1000);
	if (secs < 0) return 'just now'; // clock skew / future timestamp
	if (secs < 45) return 'just now';
	const mins = Math.round(secs / 60);
	if (mins < 60) return `${mins}m ago`;
	const hours = Math.round(mins / 60);
	if (hours < 24) return `${hours}h ago`;
	const days = Math.round(hours / 24);
	if (days < 7) return `${days}d ago`;
	const weeks = Math.round(days / 7);
	if (days < 30) return `${weeks}w ago`;
	const months = Math.round(days / 30);
	if (days < 365) return `${months}mo ago`;
	const years = Math.round(days / 365);
	return `${years}y ago`;
}

/**
 * Recover the bare workflow-instance UUID from an engine net_id, for building
 * `/instances/{id}` links.
 *
 * Multi-tenancy made instance net_ids workspace-namespaced —
 * `mekhan-{workspace_id}-{instance_id}` (was `mekhan-{instance_id}`). Both
 * segments are UUIDs, so the instance id is the LAST five hyphen-delimited
 * groups (8-4-4-4-12). Slicing the trailing groups handles the new and legacy
 * formats identically. For an execution_id (`mekhan-{ws}-{inst}-{run}`) use
 * `instanceIdFromExecution` instead. Returns null when `net` isn't a
 * mekhan-prefixed instance net (SDK/pool/infra nets have no owning instance).
 */
export function instanceIdFromNet(net: string | null | undefined): string | null {
	if (!net || !net.startsWith('mekhan-')) return null;
	const segs = net.slice('mekhan-'.length).split('-');
	if (segs.length < 5) return null;
	return segs.slice(-5).join('-');
}

/**
 * Recover the bare instance UUID from an `execution_id`
 * (`mekhan-{ws}-{inst}-{run}`). The instance id is the SECOND UUID, i.e. groups
 * 5..10 of the post-prefix segments (groups 0..5 are the workspace). Returns
 * null for anything that doesn't carry both the workspace and instance UUIDs.
 */
export function instanceIdFromExecution(execution: string | null | undefined): string | null {
	if (!execution || !execution.startsWith('mekhan-')) return null;
	const segs = execution.slice('mekhan-'.length).split('-');
	// {ws}=5 groups, {inst}=5 groups → need at least 10.
	if (segs.length < 10) return null;
	return segs.slice(5, 10).join('-');
}
