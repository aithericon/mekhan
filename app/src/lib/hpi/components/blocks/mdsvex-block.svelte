<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import { renderMdsvex } from '$lib/mdsvex';
	import { MDSVEX_CLASS } from '$lib/mdsvex-styles';
	import type { TaskBlock } from '../../types';

	let { block }: { block: Extract<TaskBlock, { type: 'mdsvex' }> } = $props();
</script>

<div class="{MDSVEX_CLASS} py-1" data-testid="step-block-mdsvex">
	<!-- eslint-disable-next-line svelte/no-at-html-tags -->
	{@html renderMdsvex(block.content)}
</div>
