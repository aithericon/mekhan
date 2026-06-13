<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import DataTable from '../data-table.svelte';
	import { resolveTableRows } from '../../utils';
	import type { TaskBlock } from '../../types';

	let { block, taskData }: {
		block: Extract<TaskBlock, { type: 'table' }>;
		/** Staged task payload — `rows_ref` paths resolve against this. */
		taskData?: Record<string, unknown>;
	} = $props();

	const rows = $derived(resolveTableRows(block, taskData));
</script>

<div data-testid="step-block-table">
	<DataTable
		headers={block.headers}
		{rows}
		alignments={block.alignments}
		caption={block.caption}
	/>
</div>
