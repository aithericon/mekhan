/**
 * Svelte context that supplies per-node step-execution data to editor node
 * components when they're rendered as part of an instance-view canvas
 * overlay. When this context is absent (the regular template editor),
 * `getNodeRuntime(id)` returns an empty list and the overlay is invisible.
 *
 * The provider is `WorkflowGraphView.svelte`; consumers are
 * `WorkflowNodeCard.svelte` (which all standard nodes compose) and
 * `LoopNode.svelte` (which doesn't use the card).
 */
import { getContext, setContext } from 'svelte';
import type { StepExecution } from '$lib/api/client';

export type NodeRuntimeLookup = (nodeId: string) => StepExecution[];

export const NODE_RUNTIME_CONTEXT_KEY = Symbol('node-runtime');

export function provideNodeRuntime(lookup: NodeRuntimeLookup): void {
	setContext(NODE_RUNTIME_CONTEXT_KEY, lookup);
}

export function useNodeRuntime(): NodeRuntimeLookup {
	const lookup = getContext<NodeRuntimeLookup | undefined>(NODE_RUNTIME_CONTEXT_KEY);
	return lookup ?? (() => []);
}
