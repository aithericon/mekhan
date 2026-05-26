<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import Callout from '../callout.svelte';
	import type { TaskBlock } from '../../types';

	let { block }: { block: Extract<TaskBlock, { type: 'callout' }> } = $props();
</script>

<div data-testid="step-block-callout">
	<Callout severity={block.severity} title={block.title} content={block.content} />
</div>
