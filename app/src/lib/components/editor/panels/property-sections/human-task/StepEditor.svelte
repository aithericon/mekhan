<script lang="ts">
	import type { TaskStepConfig, TaskBlockConfig, TaskFieldConfig } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import InputBlockEditor from './InputBlockEditor.svelte';
	import MdsvexBlockEditor from './MdsvexBlockEditor.svelte';
	import CalloutBlockEditor from './CalloutBlockEditor.svelte';
	import DividerBlockDisplay from './DividerBlockDisplay.svelte';
	import ImageBlockEditor from './ImageBlockEditor.svelte';
	import FileBlockEditor from './FileBlockEditor.svelte';
	import PdfBlockEditor from './PdfBlockEditor.svelte';
	import DownloadBlockEditor from './DownloadBlockEditor.svelte';
	import BlockTypePicker from './BlockTypePicker.svelte';

	type Props = {
		step: TaskStepConfig;
		readonly?: boolean;
		binding?: YjsGraphBinding;
		nodeId?: string;
		onchange: (step: TaskStepConfig) => void;
		onremove: () => void;
	};

	let { step, readonly = false, binding, nodeId, onchange, onremove }: Props = $props();

	function updateTitle(title: string) {
		onchange({ ...step, title });
	}

	function updateDescription(descriptionMdsvex: string) {
		onchange({ ...step, descriptionMdsvex: descriptionMdsvex || undefined });
	}

	function addBlock(block: TaskBlockConfig) {
		onchange({ ...step, blocks: [...step.blocks, block] });
	}

	function updateBlock(index: number, block: TaskBlockConfig) {
		const blocks = [...step.blocks];
		blocks[index] = block;
		onchange({ ...step, blocks });
	}

	function removeBlock(index: number) {
		onchange({ ...step, blocks: step.blocks.filter((_, i) => i !== index) });
	}
</script>

<div class="rounded-lg border border-border bg-muted/30 p-3">
	<div class="mb-3 flex items-center gap-2">
		<Input
			type="text"
			value={step.title}
			disabled={readonly}
			oninput={(e) => updateTitle((e.currentTarget as HTMLInputElement).value)}
			class="flex-1 font-medium"
		/>
		{#if !readonly}
			<button
				type="button"
				class="rounded p-1 text-muted-foreground transition-colors hover:text-destructive"
				onclick={onremove}
			>
				<Trash2 class="size-4" />
			</button>
		{/if}
	</div>

	<!-- Step description -->
	<div class="mb-3">
		<Textarea
			value={step.descriptionMdsvex ?? ''}
			placeholder="Step description (Markdown)..."
			disabled={readonly}
			oninput={(e) => updateDescription((e.currentTarget as HTMLTextAreaElement).value)}
			rows={2}
		/>
	</div>

	<!-- Blocks -->
	<div class="space-y-2">
		{#each step.blocks as block, blockIdx (blockIdx)}
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
					onchange={(content) => updateBlock(blockIdx, { type: 'mdsvex', content })}
					onremove={() => removeBlock(blockIdx)}
				/>
			{:else if block.type === 'callout'}
				<CalloutBlockEditor
					severity={block.severity}
					title={block.title ?? undefined}
					content={block.content}
					{readonly}
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
					onchange={(filename, caption, height, url) =>
						updateBlock(blockIdx, { ...block, type: 'pdf', filename, caption, height, url })}
					onremove={() => removeBlock(blockIdx)}
				/>
			{:else if block.type === 'download'}
				<DownloadBlockEditor
					downloads={block.downloads}
					{readonly}
					onchange={(downloads) =>
						updateBlock(blockIdx, { type: 'download', downloads })}
					onremove={() => removeBlock(blockIdx)}
				/>
			{/if}
		{/each}
	</div>

	{#if !readonly}
		<div class="mt-3">
			<BlockTypePicker onadd={addBlock} />
		</div>
	{/if}
</div>
