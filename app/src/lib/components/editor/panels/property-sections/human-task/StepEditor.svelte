<script lang="ts">
	import type { TaskStepConfig, TaskBlockConfig } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import Trash2 from '@lucide/svelte/icons/trash-2';
	import { Input } from '$lib/components/ui/input';
	import { Textarea } from '$lib/components/ui/textarea';
	import BlockListEditor from './BlockListEditor.svelte';
	import InsertRefButton from '../InsertRefButton.svelte';

	type Props = {
		step: TaskStepConfig;
		readonly?: boolean;
		binding?: YjsGraphBinding;
		nodeId?: string;
		scope?: ScopeEntry[];
		onchange: (step: TaskStepConfig) => void;
		onremove: () => void;
	};

	let {
		step,
		readonly = false,
		binding,
		nodeId,
		scope = [],
		onchange,
		onremove
	}: Props = $props();

	function updateTitle(title: string) {
		onchange({ ...step, title });
	}

	function updateDescription(descriptionMdsvex: string) {
		onchange({ ...step, descriptionMdsvex: descriptionMdsvex || undefined });
	}

	function appendToDescription(snippet: string) {
		const curr = step.descriptionMdsvex ?? '';
		updateDescription(curr ? `${curr} ${snippet}` : snippet);
	}

	function updateBlocks(blocks: TaskBlockConfig[]) {
		onchange({ ...step, blocks });
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
	<div class="mb-3 space-y-1.5">
		<Textarea
			value={step.descriptionMdsvex ?? ''}
			placeholder="Step description (Markdown)..."
			disabled={readonly}
			oninput={(e) => updateDescription((e.currentTarget as HTMLTextAreaElement).value)}
			rows={2}
		/>
		{#if scope.length > 0}
			<InsertRefButton {scope} disabled={readonly} oninsert={appendToDescription} />
		{/if}
	</div>

	<!-- Blocks -->
	<BlockListEditor
		blocks={step.blocks}
		{readonly}
		{binding}
		{nodeId}
		{scope}
		onchange={updateBlocks}
	/>
</div>
