<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import DownloadCard from '../download-card.svelte';
	import { getLinkId, withLinkParam } from '../link-context';
	import type { TaskBlock } from '../../types';

	let { block }: { block: Extract<TaskBlock, { type: 'download' }> } = $props();

	const linkId = getLinkId();
</script>

<div data-testid="step-block-download">
	<DownloadCard
		downloads={linkId
			? block.downloads.map((d) => ({ ...d, url: withLinkParam(d.url, linkId) }))
			: block.downloads}
	/>
</div>
