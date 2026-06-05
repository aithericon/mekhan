<script lang="ts">
	import { BaseEdge, EdgeToolbar, getBezierPath, useSvelteFlow, type EdgeProps } from '@xyflow/svelte';
	import { compileErrors } from '$lib/editor/compile-errors.svelte';
	import { EDGE_LANE_WIDTH_PX } from '$lib/editor/edge-lane';

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

	// Lane styling: tint the edge to its source port's color (stashed in
	// `data.laneColor` by toFlowEdges) and draw it as wide as the port circle so
	// it reads as a lane out of the socket. Kept subtle (translucent) when idle,
	// stronger when the edge is selected.
	const laneColor = $derived((data as { laneColor?: string } | undefined)?.laneColor);
	const laneStyle = $derived.by(() => {
		const base = laneColor ?? 'var(--border)';
		const tint = selected
			? `color-mix(in oklch, ${base} 85%, transparent)`
			: `color-mix(in oklch, ${base} 38%, transparent)`;
		return `stroke: ${tint}; stroke-width: ${EDGE_LANE_WIDTH_PX}px; stroke-linecap: round;`;
	});

	// Phase 2 typed-ports: subscribe to the publish-error store and override
	// the stroke when this edge's id is flagged. Read at render time — no top-
	// level state mutation, so no feedback loop with xyflow's bind:edges. The
	// error state keeps the lane width but paints it solid destructive.
	const compileError = $derived(compileErrors.byEdgeId.get(id));
	const effectiveStyle = $derived(
		compileError
			? `${style ?? ''}; ${laneStyle}; stroke: hsl(var(--destructive));`
			: `${style ?? ''}; ${laneStyle}`
	);

	function handleDelete(event: MouseEvent) {
		event.stopPropagation();
		deleteElements({ edges: [{ id }] });
	}
</script>

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
