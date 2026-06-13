<script lang="ts">
	// Per-block editor dispatcher shared by StepEditor (the top-level
	// HumanTask step body) and RepeaterBlockEditor (the per-row sub-body
	// inside a Repeater). Renders the variant-specific editor for each
	// `TaskBlockConfig` and appends a `BlockTypePicker` at the end. Pass
	// `excludeRepeater={true}` when used inside a Repeater so the inner
	// picker can't offer nested iteration (a compile-time hard error).
	import type { TaskBlockConfig } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import InputBlockEditor from './InputBlockEditor.svelte';
	import MdsvexBlockEditor from './MdsvexBlockEditor.svelte';
	import CalloutBlockEditor from './CalloutBlockEditor.svelte';
	import DividerBlockDisplay from './DividerBlockDisplay.svelte';
	import ImageBlockEditor from './ImageBlockEditor.svelte';
	import FileBlockEditor from './FileBlockEditor.svelte';
	import PdfBlockEditor from './PdfBlockEditor.svelte';
	import DownloadBlockEditor from './DownloadBlockEditor.svelte';
	import TableBlockEditor from './TableBlockEditor.svelte';
	import RepeaterBlockEditor from './RepeaterBlockEditor.svelte';
	import BlockTypePicker from './BlockTypePicker.svelte';

	type Props = {
		blocks: TaskBlockConfig[];
		readonly?: boolean;
		binding?: YjsGraphBinding;
		nodeId?: string;
		scope?: ScopeEntry[];
		excludeRepeater?: boolean;
		onchange: (blocks: TaskBlockConfig[]) => void;
	};

	let {
		blocks,
		readonly = false,
		binding,
		nodeId,
		scope = [],
		excludeRepeater = false,
		onchange
	}: Props = $props();

	function addBlock(block: TaskBlockConfig) {
		onchange([...blocks, block]);
	}

	function updateBlock(index: number, block: TaskBlockConfig) {
		const next = [...blocks];
		next[index] = block;
		onchange(next);
	}

	function removeBlock(index: number) {
		onchange(blocks.filter((_, i) => i !== index));
	}
</script>

<div class="space-y-2">
	{#each blocks as block, blockIdx (blockIdx)}
		{#if block.type === 'input'}
			<InputBlockEditor
				field={block.field}
				{readonly}
				onchange={(field) => updateBlock(blockIdx, { type: 'input', field })}
				onremove={() => removeBlock(blockIdx)}
			/>
		{:else if block.type === 'mdsvex'}
			<MdsvexBlockEditor
				content={block.content}
				{readonly}
				{scope}
				onchange={(content) => updateBlock(blockIdx, { type: 'mdsvex', content })}
				onremove={() => removeBlock(blockIdx)}
			/>
		{:else if block.type === 'callout'}
			<CalloutBlockEditor
				severity={block.severity}
				title={block.title ?? undefined}
				content={block.content}
				{readonly}
				{scope}
				onchange={(updated) =>
					updateBlock(blockIdx, { type: 'callout', ...updated })}
				onremove={() => removeBlock(blockIdx)}
			/>
		{:else if block.type === 'divider'}
			<DividerBlockDisplay {readonly} onremove={() => removeBlock(blockIdx)} />
		{:else if block.type === 'image'}
			<ImageBlockEditor
				filenames={block.filenames}
				display={block.display}
				url={block.url ?? undefined}
				{binding}
				{nodeId}
				{readonly}
				{scope}
				onchange={(filenames, display, url) =>
					updateBlock(blockIdx, { ...block, type: 'image', filenames, display, url })}
				onremove={() => removeBlock(blockIdx)}
			/>
		{:else if block.type === 'file'}
			<FileBlockEditor
				filename={block.filename}
				{binding}
				{nodeId}
				{readonly}
				onchange={(filename) =>
					updateBlock(blockIdx, { type: 'file', filename })}
				onremove={() => removeBlock(blockIdx)}
			/>
		{:else if block.type === 'pdf'}
			<PdfBlockEditor
				filename={block.filename ?? undefined}
				caption={block.caption ?? undefined}
				height={block.height ?? undefined}
				url={block.url ?? undefined}
				{binding}
				{nodeId}
				{readonly}
				{scope}
				onchange={(filename, caption, height, url) =>
					updateBlock(blockIdx, { ...block, type: 'pdf', filename, caption, height, url })}
				onremove={() => removeBlock(blockIdx)}
			/>
		{:else if block.type === 'download'}
			<DownloadBlockEditor
				downloads={block.downloads}
				{readonly}
				{scope}
				onchange={(downloads) =>
					updateBlock(blockIdx, { type: 'download', downloads })}
				onremove={() => removeBlock(blockIdx)}
			/>
		{:else if block.type === 'table'}
			<TableBlockEditor
				headers={block.headers}
				rows_ref={block.rows_ref ?? undefined}
				caption={block.caption ?? undefined}
				{readonly}
				{scope}
				onchange={(updated) => updateBlock(blockIdx, { ...block, type: 'table', ...updated })}
				onremove={() => removeBlock(blockIdx)}
			/>
		{:else if block.type === 'repeater'}
			<RepeaterBlockEditor
				items_ref={block.items_ref}
				item_label_ref={block.item_label_ref ?? undefined}
				blocks={block.blocks}
				output_slug={block.output_slug}
				{readonly}
				{binding}
				{nodeId}
				{scope}
				onchange={(next) =>
					updateBlock(blockIdx, { type: 'repeater', ...next })}
				onremove={() => removeBlock(blockIdx)}
			/>
		{/if}
	{/each}
</div>

{#if !readonly}
	<div class="mt-3">
		<BlockTypePicker onadd={addBlock} {excludeRepeater} />
	</div>
{/if}
