<script lang="ts">
	import type { StepExecution } from '$lib/api/client';
	import { useNodeRuntime } from './runtime-context';
	import CheckCircle2 from '@lucide/svelte/icons/check-circle-2';
	import XCircle from '@lucide/svelte/icons/x-circle';
	import Loader2 from '@lucide/svelte/icons/loader-2';
	import Circle from '@lucide/svelte/icons/circle';
	import MinusCircle from '@lucide/svelte/icons/minus-circle';
	import Hourglass from '@lucide/svelte/icons/hourglass';

	type Props = {
		nodeId: string;
		/** When true, render only an icon (no duration); used for tight headers. */
		compact?: boolean;
		/**
		 * When true, overlays a "Waiting for resource" badge alongside (or
		 * instead of) the normal runtime status. This is the M3 resource-pool
		 * contention signal: the node's `p_{id}_claim_out` token has been
		 * emitted to the pool net but the corresponding `p_{id}_grant_inbox`
		 * is still empty (no grant received yet).
		 *
		 * TODO(M3): populate from the per-instance net marking once the M3
		 * compiler lowering is deployed. The parent (WorkflowGraphView or a
		 * dedicated PoolOverlay component) should read:
		 *   - `p_{nodeId}_claim_out` → token present means claim was sent
		 *   - `p_{nodeId}_grant_inbox` → empty means grant not yet received
		 * and set `awaitingResource={true}` when claim_out has ≥1 token AND
		 * grant_inbox has 0 tokens in the INSTANCE net's marking.
		 */
		awaitingResource?: boolean;
	};

	let { nodeId, compact = false, awaitingResource = false }: Props = $props();

	const lookup = useNodeRuntime();
	const executions = $derived(lookup(nodeId));
	// For Loop body nodes the latest iteration is the most informative; for
	// non-loop nodes there's only one row.
	const latest = $derived<StepExecution | undefined>(
		executions.length === 0 ? undefined : executions[executions.length - 1]
	);
	const iterationCount = $derived(executions.length);

	function formatDuration(ms: number | null | undefined): string {
		if (ms === null || ms === undefined) return '';
		if (ms < 1000) return `${ms}ms`;
		if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
		const mins = Math.floor(ms / 60_000);
		const secs = Math.floor((ms % 60_000) / 1000);
		return `${mins}m${secs}s`;
	}

	// Hardcoded per-status colour so the JIT keeps the classes.
	const palette: Record<string, { ring: string; bg: string; text: string; label: string }> = {
		running:   { ring: 'ring-blue-300',  bg: 'bg-blue-50',   text: 'text-blue-700',   label: 'running' },
		completed: { ring: 'ring-green-300', bg: 'bg-green-50',  text: 'text-green-700',  label: 'done' },
		failed:    { ring: 'ring-red-300',   bg: 'bg-red-50',    text: 'text-red-700',    label: 'failed' },
		skipped:   { ring: 'ring-slate-300', bg: 'bg-slate-50',  text: 'text-slate-500',  label: 'skipped' },
		pending:   { ring: 'ring-gray-300',  bg: 'bg-gray-50',   text: 'text-gray-600',   label: 'pending' }
	};
</script>

<!--
  Resource-contention "Waiting for resource" badge.
  Predicate: awaitingResource=true iff (per-instance net marking):
    count(p_{nodeId}_claim_out) > 0  AND  count(p_{nodeId}_grant_inbox) == 0
  This means the node has emitted a claim to the resource pool but has not
  yet received a grant token back.
  TODO(M3): wire this from WorkflowGraphView once M3 compiler lowering is
  deployed. Until then the prop defaults to false and the badge is invisible.
-->
{#if awaitingResource}
	<span
		class="inline-flex items-center gap-1 rounded-full px-1.5 py-0.5 text-sm font-medium ring-1 ring-purple-300 bg-purple-50 text-purple-700"
		title="Waiting for resource grant — claim queued in pool net"
		data-testid="badge-awaiting-resource"
	>
		<Hourglass class="size-3" />
		{#if !compact}
			<span>waiting</span>
		{/if}
	</span>
{/if}

{#if latest}
	{@const tone = palette[latest.status] ?? palette.pending}
	<span
		class="inline-flex items-center gap-1 rounded-full px-1.5 py-0.5 text-sm font-medium ring-1 {tone.ring} {tone.bg} {tone.text}"
		title={`${tone.label}${latest.duration_ms != null ? ' · ' + formatDuration(latest.duration_ms) : ''}${iterationCount > 1 ? ' · iter ' + (iterationCount - 1) + '/' + (iterationCount - 1) : ''}`}
	>
		{#if latest.status === 'running'}
			<Loader2 class="size-3 animate-spin" />
		{:else if latest.status === 'completed'}
			<CheckCircle2 class="size-3" />
		{:else if latest.status === 'failed'}
			<XCircle class="size-3" />
		{:else if latest.status === 'skipped'}
			<MinusCircle class="size-3" />
		{:else}
			<Circle class="size-3" />
		{/if}
		{#if !compact}
			{#if iterationCount > 1}
				<span class="tabular-nums">×{iterationCount}</span>
			{:else if latest.duration_ms != null}
				<span class="tabular-nums">{formatDuration(latest.duration_ms)}</span>
			{:else}
				<span>{tone.label}</span>
			{/if}
		{/if}
	</span>
{/if}
