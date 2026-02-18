<script lang="ts">
	import { BaseEdge, EdgeToolbar, getBezierPath, useSvelteFlow, type EdgeProps } from '@xyflow/svelte';

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
		interactionWidth
	}: EdgeProps = $props();

	const { deleteElements } = useSvelteFlow();

	const pathResult = $derived(
		getBezierPath({ sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition })
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
	{style}
	{markerStart}
	{markerEnd}
	{interactionWidth}
/>

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
