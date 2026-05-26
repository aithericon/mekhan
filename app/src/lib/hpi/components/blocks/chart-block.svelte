<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import BlockChart from '$lib/components/ui/block-chart/block-chart.svelte';
	import type { TaskBlock } from '../../types';

	let { block }: { block: Extract<TaskBlock, { type: 'chart' }> } = $props();
</script>

<div data-testid="step-block-chart">
	<BlockChart
		chart_type={block.chart_type}
		data={block.data}
		x={block.x}
		series={block.series}
		caption={block.caption}
		height={block.height}
		x_label={block.x_label}
		y_label={block.y_label}
	/>
</div>
