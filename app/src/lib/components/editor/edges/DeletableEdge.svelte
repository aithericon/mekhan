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
	<EdgeLabel x={pathResult[1]} y={pathResult[2]} width={240} height={136}>
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

<style>
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
