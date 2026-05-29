/**
 * Svelte context that supplies per-node runtime data to editor node
 * components when they're rendered as part of an instance-view canvas
 * overlay. When this context is absent (the regular template editor),
 * the lookups return inert defaults and the overlay is invisible.
 *
 * Two channels:
 *  - `NodeRuntimeLookup` — `node_id → StepExecution[]` (status badge data).
 *  - `AwaitingResourceLookup` — `node_id → boolean` (the M3 resource-pool
 *    "waiting for resource" predicate, computed from the instance net
 *    marking). Provided alongside the runtime lookup by `WorkflowGraphView`
 *    so the badge can read it via context rather than being prop-drilled
 *    through xyflow node components.
 *
 * The provider is `WorkflowGraphView.svelte`; consumers are
 * `WorkflowNodeCard.svelte` (which all standard nodes compose) and
 * `LoopNode.svelte` (which doesn't use the card), via `NodeRuntimeBadge`.
 */
import { getContext, setContext } from 'svelte';
import type { StepExecution } from '$lib/api/client';

export type NodeRuntimeLookup = (nodeId: string) => StepExecution[];

/** `node_id → true` when the node has claimed a pooled resource and is
 *  waiting for the grant (see `instance-marking.svelte.ts::isAwaitingResource`). */
export type AwaitingResourceLookup = (nodeId: string) => boolean;

export const NODE_RUNTIME_CONTEXT_KEY = Symbol('node-runtime');
export const AWAITING_RESOURCE_CONTEXT_KEY = Symbol('awaiting-resource');

export function provideNodeRuntime(lookup: NodeRuntimeLookup): void {
	setContext(NODE_RUNTIME_CONTEXT_KEY, lookup);
}

export function useNodeRuntime(): NodeRuntimeLookup {
	const lookup = getContext<NodeRuntimeLookup | undefined>(NODE_RUNTIME_CONTEXT_KEY);
	return lookup ?? (() => []);
}

export function provideAwaitingResource(lookup: AwaitingResourceLookup): void {
	setContext(AWAITING_RESOURCE_CONTEXT_KEY, lookup);
}

export function useAwaitingResource(): AwaitingResourceLookup {
	const lookup = getContext<AwaitingResourceLookup | undefined>(AWAITING_RESOURCE_CONTEXT_KEY);
	return lookup ?? (() => false);
}
