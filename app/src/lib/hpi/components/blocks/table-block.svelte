<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import DataTable from '../data-table.svelte';
	import type { TaskBlock } from '../../types';

	let { block }: { block: Extract<TaskBlock, { type: 'table' }> } = $props();
</script>

<div data-testid="step-block-table">
	<DataTable
		headers={block.headers}
		rows={block.rows}
		alignments={block.alignments}
		caption={block.caption}
	/>
</div>
