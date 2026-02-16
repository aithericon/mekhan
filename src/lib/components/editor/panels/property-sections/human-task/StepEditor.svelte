<script lang="ts">
	import type { TaskStepConfig, TaskBlockConfig, TaskFieldConfig } from '$lib/types/editor';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import InputBlockEditor from './InputBlockEditor.svelte';
	import MdsvexBlockEditor from './MdsvexBlockEditor.svelte';
	import CalloutBlockEditor from './CalloutBlockEditor.svelte';
	import DividerBlockDisplay from './DividerBlockDisplay.svelte';
	import BlockTypePicker from './BlockTypePicker.svelte';

	type Props = {
		step: TaskStepConfig;
		readonly?: boolean;
		onchange: (step: TaskStepConfig) => void;
		onremove: () => void;
	};

	let { step, readonly = false, onchange, onremove }: Props = $props();

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

<div class="rounded-lg border border-border bg-muted/30 p-2">
	<div class="mb-2 flex items-center gap-2">
		<input
			type="text"
			value={step.title}
			disabled={readonly}
			oninput={(e) => updateTitle((e.currentTarget as HTMLInputElement).value)}
			class="flex-1 rounded-md border border-input bg-background px-2 py-1 text-xs text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		/>
		{#if !readonly}
			<button
				type="button"
				class="rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
				onclick={onremove}
			>
				<Trash2 class="size-3.5" />
			</button>
		{/if}
	</div>

	<!-- Step description -->
	<div class="mb-2">
		<textarea
			value={step.descriptionMdsvex ?? ''}
			placeholder="Step description (Markdown)..."
			disabled={readonly}
			oninput={(e) => updateDescription((e.currentTarget as HTMLTextAreaElement).value)}
			rows={2}
			class="w-full rounded border border-input bg-background px-1.5 py-1 text-[10px] text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
		></textarea>
	</div>

	<!-- Blocks -->
	<div class="space-y-1.5">
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
					title={block.title}
					content={block.content}
					{readonly}
					onchange={(updated) =>
						updateBlock(blockIdx, { type: 'callout', ...updated })}
					onremove={() => removeBlock(blockIdx)}
				/>
			{:else if block.type === 'divider'}
				<DividerBlockDisplay {readonly} onremove={() => removeBlock(blockIdx)} />
			{/if}
		{/each}
	</div>

	{#if !readonly}
		<div class="mt-2">
			<BlockTypePicker onadd={addBlock} />
		</div>
	{/if}
</div>
