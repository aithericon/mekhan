<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import DownloadCard from './download-card.svelte';
	import DataTable from './data-table.svelte';
	import BlockImage from './block-image.svelte';
	import BlockPdf from './block-pdf.svelte';
	import Callout from './callout.svelte';
	import { getLinkId, withLinkParam } from './link-context';
	import type { TaskBlock } from '../types';

	type NonInputBlock = Exclude<TaskBlock, { type: 'input' }>;

	let {
		block,
		renderMdsvex,
		mdsvexClass = ''
	}: {
		block: NonInputBlock;
		/** Optional callback to render mdsvex content to HTML. Falls back to a <pre> tag. */
		renderMdsvex?: (content: string) => string;
		/** CSS class applied to the mdsvex wrapper div (e.g. prose styles). */
		mdsvexClass?: string;
	} = $props();

	const linkId = getLinkId();
</script>

{#if block.type === 'mdsvex'}
	<div class="{mdsvexClass} py-1" data-testid="step-block-mdsvex">
		{#if renderMdsvex}
			<!-- eslint-disable-next-line svelte/no-at-html-tags -->
			{@html renderMdsvex(block.content)}
		{:else}
			<pre class="whitespace-pre-wrap text-sm text-foreground">{block.content}</pre>
		{/if}
	</div>
{:else if block.type === 'download'}
	<div data-testid="step-block-download">
		<DownloadCard
			downloads={linkId
				? block.downloads.map((d) => ({ ...d, url: withLinkParam(d.url, linkId) }))
				: block.downloads}
		/>
	</div>
{:else if block.type === 'table'}
	<div data-testid="step-block-table">
		<DataTable
			headers={block.headers}
			rows={block.rows}
			alignments={block.alignments}
			caption={block.caption}
		/>
	</div>
{:else if block.type === 'image'}
	<div data-testid="step-block-image">
		<BlockImage url={withLinkParam(block.url, linkId)} alt={block.alt} caption={block.caption} />
	</div>
{:else if block.type === 'callout'}
	<div data-testid="step-block-callout">
		<Callout severity={block.severity} title={block.title} content={block.content} />
	</div>
{:else if block.type === 'pdf'}
	<div data-testid="step-block-pdf">
		<BlockPdf
			url={withLinkParam(block.url, linkId)}
			filename={block.filename}
			caption={block.caption}
			height={block.height}
		/>
	</div>
{:else if block.type === 'chart'}
	<!-- Chart rendering requires a host-provided component; not included in hpi-ui -->
	<div data-testid="step-block-chart" class="rounded-xl border border-border bg-card/70 p-4 text-sm text-muted-foreground">
		Chart: {block.chart_type} ({block.caption ?? 'no caption'})
	</div>
{:else if block.type === 'divider'}
	<hr class="my-4 border-border/50" data-testid="step-block-divider" />
{/if}
