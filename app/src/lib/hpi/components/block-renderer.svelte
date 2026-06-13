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

	// Repeater is intentionally NOT a NonInputBlock renderer target —
	// it carries an interactive sub-form (Feature B). TaskForm.svelte
	// handles it inline alongside Input blocks, sharing the same
	// form-state machinery. BlockRenderer is only for display blocks.
	type NonInputBlock = Exclude<TaskBlock, { type: 'input' } | { type: 'repeater' }>;

	let { block, taskData }: {
		block: NonInputBlock;
		/** Staged task payload — table blocks resolve `rows_ref` against it. */
		taskData?: Record<string, unknown>;
	} = $props();

	// `satisfies` enforces exhaustiveness: every NonInputBlock variant must have
	// a renderer whose `block` prop matches the variant's shape (taskData is
	// optional — renderers that don't consume it still satisfy). A missing or
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
		[K in NonInputBlock['type']]: Component<{
			block: Extract<NonInputBlock, { type: K }>;
			taskData?: Record<string, unknown>;
		}>;
	};

	const Renderer = $derived(
		RENDERERS[block.type] as Component<{ block: NonInputBlock; taskData?: Record<string, unknown> }>
	);
</script>

<Renderer {block} {taskData} />
