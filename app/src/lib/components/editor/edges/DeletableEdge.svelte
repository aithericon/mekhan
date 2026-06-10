<script lang="ts">
	import {
		BaseEdge,
		EdgeLabel,
		EdgeToolbar,
		getBezierPath,
		useSvelteFlow,
		type EdgeProps
	} from '@xyflow/svelte';
	import { compileErrors } from '$lib/editor/compile-errors.svelte';
	import { EDGE_LANE_WIDTH_PX } from '$lib/editor/edge-lane';
	import { useEdgeFeeds } from '$lib/components/instances/edge-feed-context';
	import EdgeMediaWidget from '$lib/components/instances/EdgeMediaWidget.svelte';
	import { useEdgeJoin } from './edge-join-context';

	let {
		id,
		sourceX,
		sourceY,
		targetX,
		targetY,
		sourcePosition,
		targetPosition,
		label,
		labelStyle,
		style,
		markerStart,
		markerEnd,
		deletable,
		interactionWidth,
		data,
		selected
	}: EdgeProps = $props();

	const { deleteElements } = useSvelteFlow();

	const pathResult = $derived(
		getBezierPath({ sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition })
	);

	// Lane styling: draw the edge as wide as the port circle and tint it with a
	// gradient running from the source port's color to the target port's color
	// (both stashed on `data` by toFlowEdges) so it reads as a lane between the
	// two sockets. The gradient axis is anchored to the edge's endpoints in flow
	// space. Kept subtle (translucent) when idle, stronger when selected.
	const lane = $derived(data as { laneFrom?: string; laneTo?: string } | undefined);
	const laneFrom = $derived(lane?.laneFrom ?? 'var(--border)');
	const laneTo = $derived(lane?.laneTo ?? laneFrom);
	const stopOpacity = $derived(selected ? 0.85 : 0.4);
	// SVG ids can't contain whitespace; edge ids are generated and shouldn't, but
	// sanitize defensively so `url(#…)` always resolves.
	const gradientId = $derived(`edge-lane-${id.replace(/[^a-zA-Z0-9_-]/g, '_')}`);

	// Phase 2 typed-ports: subscribe to the publish-error store and override
	// the stroke when this edge's id is flagged. Read at render time — no top-
	// level state mutation, so no feedback loop with xyflow's bind:edges. The
	// error state keeps the lane width but paints it solid destructive (no
	// gradient) so failures stand out against the tinted lanes.
	const compileError = $derived(compileErrors.byEdgeId.get(id));
	const laneStyle = $derived(
		`stroke-width: ${EDGE_LANE_WIDTH_PX}px; stroke-linecap: round;`
	);
	const effectiveStyle = $derived(
		compileError
			? `${style ?? ''}; ${laneStyle} stroke: hsl(var(--destructive));`
			: `${style ?? ''}; ${laneStyle} stroke: url(#${gradientId});`
	);

	// In the instance/run view, the surrounding `WorkflowGraphView` provides an
	// edge-feed lookup; in the plain template editor there's NO provider, so
	// `feedGetter` is undefined and nothing extra renders (editor unchanged).
	const feedGetter = useEdgeFeeds();
	const feed = $derived(feedGetter ? feedGetter(id) : null);

	// Control-channel join chip (docs/25): a consumer edge tapping a CONTROL
	// channel carries the per-edge join discipline. `channelPlane`/`join` are
	// stashed on `data` by toFlowEdges; the setter context is provided only by
	// editable canvases (WorkflowCanvas, not readonly) — when absent (instance
	// view / readonly) the chip renders display-only.
	const joinData = $derived(data as { channelPlane?: string; join?: string } | undefined);
	const isControlChannel = $derived(joinData?.channelPlane === 'control');
	const join = $derived(joinData?.join === 'gather' ? 'gather' : 'each');
	const setEdgeJoin = useEdgeJoin();
	const JOIN_TITLE =
		'each — one downstream firing per emitted item\n' +
		'gather — items collected into one array on close';

	function toggleJoin(event: MouseEvent) {
		event.stopPropagation();
		setEdgeJoin?.(id, join === 'gather' ? null : 'gather');
	}

	function handleDelete(event: MouseEvent) {
		event.stopPropagation();
		deleteElements({ edges: [{ id }] });
	}
</script>

{#if !compileError}
	<defs>
		<linearGradient
			id={gradientId}
			gradientUnits="userSpaceOnUse"
			x1={sourceX}
			y1={sourceY}
			x2={targetX}
			y2={targetY}
		>
			<stop offset="0%" stop-color={laneFrom} stop-opacity={stopOpacity} />
			<stop offset="100%" stop-color={laneTo} stop-opacity={stopOpacity} />
		</linearGradient>
	</defs>
{/if}
<BaseEdge
	path={pathResult[0]}
	labelX={pathResult[1]}
	labelY={pathResult[2]}
	{label}
	{labelStyle}
	style={effectiveStyle}
	{markerStart}
	{markerEnd}
	{interactionWidth}
/>
{#if compileError}
	<title>{compileError.message}</title>
{/if}

{#if feed}
	<!-- Only claim the full media box for feeds with a live renderer. A
	     plan-null feed (data-plane binary with no renderer, e.g. text/plain)
	     collapses to a tiny liveness dot — a fixed 320×180 label would paint
	     an empty var(--card) (near-black in dark mode) rectangle on the edge. -->
	<EdgeLabel
		x={pathResult[1]}
		y={pathResult[2]}
		width={feed.plan ? 320 : undefined}
		height={feed.plan ? 180 : undefined}
		transparent={!feed.plan}
	>
		<EdgeMediaWidget {feed} />
	</EdgeLabel>
{/if}

{#if deletable !== false}
	<EdgeToolbar x={pathResult[1]} y={pathResult[2]} isVisible>
		<div class="edge-delete-zone">
			<button
				class="edge-delete-btn"
				onclick={handleDelete}
				aria-label="Delete connection"
			>
				&times;
			</button>
		</div>
	</EdgeToolbar>
{/if}

<!-- Control-channel join chip. Rendered AFTER the delete toolbar (both portal
     into the same `edge-labels` layer, so DOM order is paint order) and nudged
     below the midpoint so the hover-delete × stays reachable. -->
{#if isControlChannel}
	<EdgeLabel x={pathResult[1]} y={pathResult[2] + 22}>
		{#if setEdgeJoin}
			<button class="edge-join-chip" onclick={toggleJoin} title={JOIN_TITLE}>
				{join}
			</button>
		{:else}
			<span class="edge-join-chip is-static" title={JOIN_TITLE}>{join}</span>
		{/if}
	</EdgeLabel>
{/if}

<style>
	/* Join-discipline chip — control-channel purple (#a855f7), matching the
	   channel handle + lane tint. Compact pill, same visual family as the
	   node-face channel chips. */
	.edge-join-chip {
		display: inline-flex;
		align-items: center;
		padding: 1px 7px;
		border-radius: 9999px;
		border: 1px solid #a855f7;
		background: hsl(var(--background));
		color: #a855f7;
		font-size: 10px;
		font-weight: 600;
		line-height: 1.5;
		cursor: pointer;
	}

	.edge-join-chip:hover {
		background: #a855f7;
		color: white;
	}

	.edge-join-chip.is-static {
		cursor: default;
	}

	.edge-join-chip.is-static:hover {
		background: hsl(var(--background));
		color: #a855f7;
	}

	.edge-delete-zone {
		width: 80px;
		height: 80px;
		display: flex;
		align-items: center;
		justify-content: center;
	}

	.edge-delete-btn {
		width: 64px;
		height: 64px;
		border-radius: 50%;
		border: 2px solid hsl(var(--border));
		background: hsl(var(--background));
		color: hsl(var(--muted-foreground));
		font-size: 44px;
		line-height: 1;
		display: flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
		padding: 0;
		opacity: 0;
		transition:
			opacity 150ms,
			background 150ms,
			color 150ms,
			border-color 150ms;
	}

	.edge-delete-zone:hover .edge-delete-btn {
		opacity: 1;
	}

	.edge-delete-btn:hover {
		background: hsl(var(--destructive));
		color: hsl(var(--destructive-foreground));
		border-color: hsl(var(--destructive));
	}
</style>
