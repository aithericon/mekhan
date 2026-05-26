<script lang="ts">
	// SPDX-License-Identifier: Apache-2.0
	import type { Component } from 'svelte';
	import MdsvexBlock from './blocks/mdsvex-block.svelte';
	import DownloadBlock from './blocks/download-block.svelte';
	import TableBlock from './blocks/table-block.svelte';
	import ImageBlock from './blocks/image-block.svelte';
	import CalloutBlock from './blocks/callout-block.svelte';
	import PdfBlock from './blocks/pdf-block.svelte';
	import ChartBlock from './blocks/chart-block.svelte';
	import DividerBlock from './blocks/divider-block.svelte';
	import type { TaskBlock } from '../types';

	type NonInputBlock = Exclude<TaskBlock, { type: 'input' }>;

	let { block }: { block: NonInputBlock } = $props();

	// `satisfies` enforces exhaustiveness: every NonInputBlock variant must have
	// a renderer whose `block` prop matches the variant's shape. A missing or
	// mistyped entry fails to compile.
	const RENDERERS = {
		mdsvex: MdsvexBlock,
		download: DownloadBlock,
		table: TableBlock,
		image: ImageBlock,
		callout: CalloutBlock,
		pdf: PdfBlock,
		chart: ChartBlock,
		divider: DividerBlock
	} satisfies {
		[K in NonInputBlock['type']]: Component<{ block: Extract<NonInputBlock, { type: K }> }>;
	};

	const Renderer = $derived(RENDERERS[block.type] as Component<{ block: NonInputBlock }>);
</script>

<Renderer {block} />
