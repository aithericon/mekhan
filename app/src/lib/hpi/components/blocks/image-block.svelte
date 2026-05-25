<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import BlockImage from '../block-image.svelte';
	import { getLinkId, withLinkParam } from '../link-context';
	import type { TaskBlock } from '../../types';

	let { block }: { block: Extract<TaskBlock, { type: 'image' }> } = $props();

	const linkId = getLinkId();
</script>

<div data-testid="step-block-image">
	<BlockImage url={withLinkParam(block.url, linkId)} alt={block.alt} caption={block.caption} />
</div>
