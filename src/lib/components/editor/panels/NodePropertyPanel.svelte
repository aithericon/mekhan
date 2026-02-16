<script lang="ts">
	import type { WorkflowNodeData } from '$lib/types/editor';
	import X from '@lucide/svelte/icons/x';
	import StartNodeSection from './property-sections/StartNodeSection.svelte';
	import HumanTaskSection from './property-sections/HumanTaskSection.svelte';
	import AutomatedStepSection from './property-sections/AutomatedStepSection.svelte';
	import DecisionNodeSection from './property-sections/DecisionNodeSection.svelte';
	import LoopNodeSection from './property-sections/LoopNodeSection.svelte';

	type Props = {
		data: WorkflowNodeData;
		readonly?: boolean;
		onchange: (data: WorkflowNodeData) => void;
		onclose: () => void;
	};

	let { data, readonly = false, onchange, onclose }: Props = $props();

	function updateField<K extends keyof WorkflowNodeData>(
		key: K,
		value: WorkflowNodeData[K]
	) {
		onchange({ ...data, [key]: value } as WorkflowNodeData);
	}
</script>

<div class="flex w-80 flex-col border-l border-border bg-card" data-testid="node-property-panel">
	<div class="flex items-center justify-between border-b border-border px-3 py-2.5">
		<h2 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
			{readonly ? 'Inspector' : 'Properties'}
		</h2>
		<button
			type="button"
			class="rounded-md p-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
			data-testid="btn-close-properties"
			onclick={onclose}
		>
			<X class="size-4" />
		</button>
	</div>

	<div class="flex-1 space-y-4 overflow-y-auto p-3">
		<!-- Common: Label -->
		<div class="space-y-1.5">
			<label for="node-label" class="text-xs font-medium text-muted-foreground">Label</label>
			<input
				id="node-label"
				type="text"
				value={data.label}
				disabled={readonly}
				data-testid="input-node-label"
				oninput={(e) => updateField('label', (e.currentTarget as HTMLInputElement).value)}
				class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
			/>
		</div>

		<!-- Common: Description -->
		<div class="space-y-1.5">
			<label for="node-desc" class="text-xs font-medium text-muted-foreground">Description</label>
			<textarea
				id="node-desc"
				value={data.description ?? ''}
				disabled={readonly}
				data-testid="input-node-description"
				oninput={(e) => updateField('description', (e.currentTarget as HTMLTextAreaElement).value)}
				rows={2}
				class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
			></textarea>
		</div>

		<!-- Type-specific sections -->
		{#if data.type === 'start'}
			<StartNodeSection {data} {readonly} {onchange} />
		{:else if data.type === 'human_task'}
			<HumanTaskSection {data} {readonly} {onchange} />
		{:else if data.type === 'automated_step'}
			<AutomatedStepSection {data} {readonly} {onchange} />
		{:else if data.type === 'decision'}
			<DecisionNodeSection {data} {readonly} {onchange} />
		{:else if data.type === 'loop'}
			<LoopNodeSection {data} {readonly} {onchange} />
		{/if}
	</div>
</div>
