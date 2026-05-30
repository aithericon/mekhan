<script lang="ts">
	import { getContext } from 'svelte';
	import { Handle, NodeResizer, Position } from '@xyflow/svelte';
	import type { LeaseScopeNodeData } from '$lib/types/editor';
	import Lock from '@lucide/svelte/icons/lock';
	import NodeRuntimeBadge from '$lib/components/instances/NodeRuntimeBadge.svelte';
	import {
		RESIZE_REPORT_CONTEXT_KEY,
		type ResizeReport
	} from './resize-context';

	let { id, data, selected }: { id: string; data: LeaseScopeNodeData; selected?: boolean } = $props();

	// Absent on readonly canvases / standalone previews — fall back to no-op
	// so the resizer never crashes if rendered outside WorkflowCanvas.
	const reportResize = getContext<ResizeReport | undefined>(RESIZE_REPORT_CONTEXT_KEY);

	// Header reads `<label> · <datacenter alias>`. The alias is the held
	// allocation's source; an unset alias (freshly dropped) renders as a muted
	// "no datacenter" prompt that the property panel resolves.
	let schedulerText = $derived.by(() => {
		const s = (data.lease?.scheduler ?? '').trim();
		return s || 'no datacenter';
	});
</script>

<!--
	LeaseScope is a container: any child node placed inside its bounds (with
	`parentId == lease_scope.id`) runs ON the held allocation. The scope
	acquires the lease on enter and releases it on exit — there is no per-step
	flag (run_on_lease is retired). The compiler routes the control token in via
	`body_in` (source handle, interior-right edge) and back out via `body_out`
	(target handle, interior-left edge); the outer `in`/`out` handles sit on the
	perimeter and connect to the parent flow. Mirrors LoopNode's container
	handle convention so the editor's drop/reparent + body-return logic apply
	unchanged.
-->
<NodeResizer
	isVisible={selected && !!reportResize}
	minWidth={240}
	minHeight={150}
	onResizeEnd={(_e, params) => reportResize?.(id, params)}
/>

<!--
	Outer perimeter handles: connect the scope to the parent flow. Outline
	circles (bg-card fill, colored border) to distinguish them from the filled
	interior body handles below.
-->
<Handle
	id="in"
	type="target"
	position={Position.Left}
	class="!h-3 !w-3 !border-2 !border-node-lease-scope !bg-card"
	title="Flow in — acquire the lease, enter the scope"
/>
<Handle
	id="out"
	type="source"
	position={Position.Right}
	class="!h-3 !w-3 !border-2 !border-node-lease-scope !bg-card"
	title="Flow out — release the lease, continue after the scope"
/>

<div
	class="relative h-full w-full rounded-xl border-2 border-dashed {selected
		? 'border-node-lease-scope bg-node-lease-scope/10'
		: 'border-node-lease-scope/50 bg-node-lease-scope/5'}"
	data-testid="node-lease-scope"
>
	<!--
		Title bar — top inside edge so it doesn't eat the perimeter handle area.
		Reads `<label> · <datacenter alias>`, with the alias muted as a
		secondary suffix.
	-->
	<div class="absolute left-3 right-3 top-2 flex items-center gap-2 text-sm font-medium text-muted-foreground">
		<Lock class="size-4 text-node-lease-scope" />
		<span>{data.label}</span>
		<span class="font-mono text-sm opacity-60">· {schedulerText}</span>
		<div class="ml-auto">
			<NodeRuntimeBadge nodeId={id} />
		</div>
	</div>

	<!--
		Inner-facing handles. `body_in` is the source side (scope hands the
		control token to the body); `body_out` is the target side (body returns
		it to the scope on exit). Positioned 16px inside the container edges with
		a SOLID fill (vs the outline perimeter handles) so the four ports are
		visually unambiguous. `position` points INWARD, not at the wall the
		handle sits on; `left/right: auto` overrides stop the position class's
		default wall-anchor from fighting the styled placement.
	-->
	<Handle
		id="body_in"
		type="source"
		position={Position.Right}
		class="!h-3 !w-3 !border-2 !border-node-lease-scope !bg-node-lease-scope"
		style="left: 16px; right: auto; top: 50%;"
		title="Body in — enter the held allocation"
	/>
	<Handle
		id="body_out"
		type="target"
		position={Position.Left}
		class="!h-3 !w-3 !border-2 !border-node-lease-scope !bg-node-lease-scope"
		style="right: 16px; left: auto; top: 50%;"
		title="Body out — leave the scope (releases the lease)"
	/>
</div>
