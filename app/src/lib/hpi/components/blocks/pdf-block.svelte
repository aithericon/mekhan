<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import BlockPdf from '../block-pdf.svelte';
	import { getLinkId, withLinkParam } from '../link-context';
	import type { TaskBlock } from '../../types';

	let { block }: { block: Extract<TaskBlock, { type: 'pdf' }> } = $props();

	const linkId = getLinkId();
</script>

<div data-testid="step-block-pdf">
	<BlockPdf
		url={withLinkParam(block.url, linkId)}
		filename={block.filename}
		caption={block.caption}
		height={block.height}
	/>
</div>
