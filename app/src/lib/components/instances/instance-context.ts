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
