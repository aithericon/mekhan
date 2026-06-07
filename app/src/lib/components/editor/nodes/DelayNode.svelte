<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { DelayNodeData } from '$lib/types/editor';
	import Timer from '@lucide/svelte/icons/timer';
	import WorkflowNodeCard, { workflowNodeHandleClass } from './WorkflowNodeCard.svelte';
	import { NODE_WIDTH } from '$lib/editor/node-dimensions';

	let { id, data, selected }: { id: string; data: DelayNodeData; selected?: boolean } = $props();

	const EXPR_MAX = 24;
	let exprText = $derived.by(() => {
		const e = (data.durationMsExpr ?? '').trim();
		if (!e) return 'no duration';
		return e.length > EXPR_MAX ? `${e.slice(0, EXPR_MAX - 1)}…` : e;
	});
</script>

<Handle id="in" type="target" position={Position.Left} class={workflowNodeHandleClass('delay')} />
<WorkflowNodeCard
	nodeId={id}
	kind="delay"
	icon={Timer}
	label={data.label}
	{selected}
	width={NODE_WIDTH.delay}
	data-testid="node-delay"
	body={delayBody}
/>
{#snippet delayBody()}
	<span class="font-mono">{exprText} ms</span>
{/snippet}
<Handle type="source" position={Position.Right} class={workflowNodeHandleClass('delay')} />
