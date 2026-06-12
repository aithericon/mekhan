/**
 * Shared instance-page state, set in `/instances/[id]/+layout.svelte` and
 * consumed by every tab subroute. Holds the loaded `WorkflowInstance` and its
 * associated processes so each subpage doesn't re-fetch them on tab switch.
 * Mutating fields on the returned object is reactive (Svelte 5 deep $state).
 */
import { getContext, setContext } from 'svelte';
import type { HpiProcess, WorkflowInstance } from '$lib/api/client';

export type InstanceContext = {
	instanceId: string;
	instance: WorkflowInstance | null;
	processes: HpiProcess[];
	loading: boolean;
	error: string | null;
	/** Re-fetch instance + processes. Subpages call this after actions
	 *  (e.g. Cancel) that may change instance status. */
	reload: () => Promise<void>;
	/**
	 * Monotonically-incrementing counter the layout bumps on each NON-NOISE
	 * domain event from its already-open instance SSE stream. The graph view
	 * ($effect on this value) turns it into a coalesced projection refetch
	 * instead of blind-polling — so an idle run stops hammering the API and a
	 * live run updates sub-second. Reading it is reactive (Svelte 5 deep
	 * $state). The high-frequency per-frame noise events (TokenCreated /
	 * EffectCompleted) are excluded by the layout, so they never bump this.
	 */
	structuralEventTick: number;
};

const KEY = Symbol('instance-context');

export function provideInstanceContext(ctx: InstanceContext): void {
	setContext(KEY, ctx);
}

export function useInstanceContext(): InstanceContext {
	const ctx = getContext<InstanceContext | undefined>(KEY);
	if (!ctx) {
		throw new Error(
			'useInstanceContext() called outside of /instances/[id] layout'
		);
	}
	return ctx;
}

/**
 * Non-throwing variant: returns the context if present, else null. The graph
 * view uses this to read the live SSE-driven `structuralEventTick` when mounted
 * under the layout, and to fall back to plain polling if it's ever mounted
 * standalone (e.g. in isolation / a future embed) where no layout provided one.
 */
export function tryUseInstanceContext(): InstanceContext | null {
	return getContext<InstanceContext | undefined>(KEY) ?? null;
}
