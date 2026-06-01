<script lang="ts">
	import { getContext } from 'svelte';
	import { Handle, NodeResizer, Position } from '@xyflow/svelte';
	import type { MapNodeData } from '$lib/types/editor';
	import MapIcon from '@lucide/svelte/icons/map';
	import NodeRuntimeBadge from '$lib/components/instances/NodeRuntimeBadge.svelte';
	import {
		RESIZE_REPORT_CONTEXT_KEY,
		type ResizeReport
	} from './resize-context';

	let { id, data, selected }: { id: string; data: MapNodeData; selected?: boolean } = $props();

	// Absent on readonly canvases / standalone previews — fall back to no-op
	// so the resizer never crashes if rendered outside WorkflowCanvas.
	const reportResize = getContext<ResizeReport | undefined>(RESIZE_REPORT_CONTEXT_KEY);

	// Header reads as `<label> · <itemsRef> → [<resultVar>]` — the data-parallel
	// shape at a glance: scatter the items array, collect one `resultVar` per
	// element into a collection borrowable downstream as `<map>[*].<field>`.
	const REF_MAX = 22;
	let itemsText = $derived.by(() => {
		if (data.streamSource) return 'stream';
		const r = (data.itemsRef ?? '').trim();
		if (!r) return 'items';
		return r.length > REF_MAX ? `${r.slice(0, REF_MAX - 1)}…` : r;
	});
	let resultText = $derived((data.resultVar ?? '').trim() || 'result');
</script>

<!--
	Map is a container: body authoring places child nodes inside its bounds
	with `parentId == map.id`. The compiler scatters the `itemsRef` array into
	one body token per element (the element bound to `<itemVar>` on the token),
	routes each in via `body_in` (source handle, interior-right) and the
	per-iteration output back via `body_out` (target handle, interior-left),
	then gathers exactly N results into the parked collection. The outer
	`in`/`out` handles sit on the perimeter and connect to the parent flow.
-->
<NodeResizer
	isVisible={selected && !!reportResize}
	minWidth={220}
	minHeight={140}
	onResizeEnd={(_e, params) => reportResize?.(id, params)}
/>

<!--
	Outer perimeter handles: connect the map to the parent flow. Rendered as
	outline circles (bg-card fill, colored border) to distinguish them from the
	filled body handles below.
-->
{#if data.streamSource}
	<!-- Streaming source: ingest the producer's chunks (stream) + its EOS/count
	     token (control), instead of scattering a static itemsRef array. -->
	<Handle
		id="stream"
		type="target"
		position={Position.Left}
		style="top:35%;background:#06b6d4;border-color:#0891b2;"
		title="Stream in — chunks from a streamOutput producer"
	/>
	<Handle
		id="control"
		type="target"
		position={Position.Left}
		style="top:65%;background:#06b6d4;border-color:#0891b2;"
		title="Control in — producer EOS/completion token (sizes the gather)"
	/>
{:else}
	<Handle
		id="in"
		type="target"
		position={Position.Left}
		class="!h-3 !w-3 !border-2 !bg-card !border-node-map"
		title="Flow in — enter map"
	/>
{/if}
<Handle
	id="out"
	type="source"
	position={Position.Right}
	class="!h-3 !w-3 !border-2 !bg-card !border-node-map"
	title="Flow out — continue after map (collection ready)"
/>

<div
	class="relative h-full w-full rounded-xl border-2 border-dashed {selected
		? 'border-node-map bg-node-map/10'
		: 'border-node-map/50 bg-node-map/5'}"
	data-testid="node-map"
>
	<!--
		Title bar — sits in the top inside edge so it doesn't eat the perimeter
		handle area. Reads as `<label> · <itemsRef> → [<resultVar>]`.
	-->
	<div class="absolute left-3 right-3 top-2 flex items-center gap-2 text-sm font-medium text-muted-foreground">
		<MapIcon class="size-4 text-node-map" />
		<span>{data.label}</span>
		<span class="font-mono text-sm">· {itemsText} → [{resultText}]</span>
		<div class="ml-auto">
			<NodeRuntimeBadge nodeId={id} />
		</div>
	</div>

	<!--
		Inner-facing handles. `body_in` is the source side (map hands a per-item
		token to the body); `body_out` is the target side (body returns its
		output to the map gather). Positioned 16px inside the container's
		left/right edges and styled with a SOLID fill (vs the outline-style
		outer perimeter handles) so the four ports are visually unambiguous.

		Their `position` points INWARD, not at the wall they sit on: body_in
		uses Position.Right so its connection leaves toward the body interior,
		and body_out uses Position.Left so the return arc enters from the
		interior. The `left/right: auto` overrides stop the position class's
		default wall-anchor from fighting the styled placement.
	-->
	<Handle
		id="body_in"
		type="source"
		position={Position.Right}
		class="!h-3 !w-3 !border-2 !border-node-map !bg-node-map"
		style="left: 16px; right: auto; top: 50%;"
		title="Body in — start of each item"
	/>
	<Handle
		id="body_out"
		type="target"
		position={Position.Left}
		class="!h-3 !w-3 !border-2 !border-node-map !bg-node-map"
		style="right: 16px; left: auto; top: 50%;"
		title="Body out — per-item result (gathered into the collection)"
	/>
</div>
