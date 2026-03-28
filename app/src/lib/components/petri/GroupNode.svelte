<script lang="ts">
	interface GroupNodeData {
		label: string;
		depth?: number;
		metadata?: Record<string, unknown>;
		spotlightDimmed?: boolean;
	}

	interface Props {
		data: GroupNodeData;
	}

	let { data }: Props = $props();
</script>

<div
	class="group-node {data.spotlightDimmed ? 'spotlight-dimmed' : ''}"
	class:nested={data.depth && data.depth > 0}
>
	<div class="group-label">
		{data.label}
	</div>
</div>

<style>
	.group-node {
		width: 100%;
		height: 100%;
		background: color-mix(in srgb, var(--primary) 4%, transparent);
		border: 1px solid color-mix(in srgb, var(--primary) 18%, transparent);
		border-radius: 10px;
		pointer-events: none;
		position: relative;
	}

	.group-node.nested {
		background: color-mix(in srgb, var(--primary) 3%, transparent);
		border-color: color-mix(in srgb, var(--primary) 12%, transparent);
		border-style: dashed;
	}

	.group-label {
		position: absolute;
		top: -8px;
		left: 10px;
		background: var(--card);
		border: 1px solid color-mix(in srgb, var(--primary) 25%, transparent);
		border-radius: 4px;
		padding: 0px 6px;
		font-size: 10px;
		font-weight: 600;
		letter-spacing: 0.01em;
		color: var(--primary);
		pointer-events: auto;
		white-space: nowrap;
		line-height: 16px;
	}

	.nested .group-label {
		font-size: 9px;
		font-weight: 500;
		border-color: color-mix(in srgb, var(--primary) 15%, transparent);
		color: color-mix(in srgb, var(--primary) 70%, var(--muted-foreground));
	}

	.spotlight-dimmed {
		opacity: 0.2;
		transition: opacity 0.3s ease;
	}
</style>
