<script lang="ts">
	import { Handle, Position } from '@xyflow/svelte';
	import type { StreamConsumerNodeData } from '$lib/types/editor';
	import Merge from '@lucide/svelte/icons/merge';
	import NodeRuntimeBadge from '$lib/components/instances/NodeRuntimeBadge.svelte';

	let { id, data, selected }: { id: string; data: StreamConsumerNodeData; selected?: boolean } = $props();

	const reduceLabel = $derived.by(() => {
		const r = data.reduce ?? { kind: 'array' };
		switch (r.kind) {
			case 'array': return 'array';
			case 'concat': return r.sep != null ? `concat("${r.sep}")` : 'concat';
			case 'sum': return 'sum';
			case 'custom': return 'custom';
		}
	});

	// Body-dispatch modes run a Python child per chunk — surface the body
	// attach handles (mirrors the Map container) so the body can be wired.
	const dispatchMode = $derived((data.dispatch ?? { mode: 'rhai' }).mode);
	const hasBody = $derived(dispatchMode === 'sequentialBody' || dispatchMode === 'parallelBody');
</script>

<!--
	StreamConsumer drains the "stream" side-channel emitted by an upstream
	AutomatedStep with `streamOutput: true`. It has two inbound handles:
	  • stream  — the stream side-channel (left, top-offset)
	  • control — the control-flow token (left, bottom-offset)
	and one outbound handle:
	  • out     — continues after all chunks are consumed and reduced

	Handle ids MUST match the compiler's `lower/stream_consumer.rs` wiring.
-->
<Handle
	id="stream"
	type="target"
	position={Position.Left}
	style="top:35%;background:#06b6d4;border-color:#0891b2;"
	title="Stream in — chunks from the upstream stream-output handle"
/>
<Handle
	id="control"
	type="target"
	position={Position.Left}
	style="top:65%;background:#06b6d4;border-color:#0891b2;"
	title="Control in — main flow token (gates drain start)"
/>

<div
	class="rounded-xl border-2 shadow-sm bg-card {selected
		? 'border-cyan-400 shadow-md'
		: 'border-cyan-500/60'} min-w-[180px]"
	data-testid="node-stream-consumer"
>
	<div class="flex items-center gap-2 px-3 py-2">
		<div class="flex size-6 items-center justify-center rounded-md bg-cyan-500">
			<Merge class="size-3.5 text-white" />
		</div>
		<span class="text-sm font-medium text-foreground">{data.label}</span>
		<div class="ml-auto flex items-center gap-1.5">
			<span class="rounded bg-cyan-500/15 px-1.5 py-0.5 font-mono text-sm font-medium text-cyan-600">
				{reduceLabel}
			</span>
			<NodeRuntimeBadge nodeId={id} />
		</div>
	</div>
	{#if data.resultVar}
		<div class="border-t border-cyan-500/20 px-3 py-2 text-sm text-muted-foreground">
			<span class="font-mono">{data.resultVar}</span>
		</div>
	{/if}
</div>

<Handle
	id="out"
	type="source"
	position={Position.Right}
	style="background:#06b6d4;border-color:#0891b2;"
	title="Flow out — continues after all stream chunks are drained and reduced"
/>

<!--
	Body-attach handles — only when a per-chunk Python body is dispatched
	(SequentialBody / ParallelBody). `body_in` hands each drained chunk to the
	body child; `body_out` receives the per-chunk result (gathered + reduced).
	Hidden for Rhai (no body) and LiveReduce (one long-lived loop). Handle ids
	MUST match the compiler's `lower/stream_consumer.rs` body wiring.
-->
{#if hasBody}
	<Handle
		id="body_in"
		type="source"
		position={Position.Bottom}
		style="left:30%;background:#06b6d4;border-color:#0891b2;"
		title="Body in — each drained chunk starts a per-chunk Python body"
	/>
	<Handle
		id="body_out"
		type="target"
		position={Position.Bottom}
		style="left:70%;background:#06b6d4;border-color:#0891b2;"
		title="Body out — per-chunk body result (gathered + reduced)"
	/>
{/if}
