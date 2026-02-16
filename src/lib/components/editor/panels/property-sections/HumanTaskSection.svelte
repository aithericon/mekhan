<script lang="ts">
	import type { HumanTaskNodeData } from '$lib/types/editor';
	import Plus from '@lucide/svelte/icons/plus';
	import StepEditor from './human-task/StepEditor.svelte';

	type Props = {
		data: HumanTaskNodeData;
		readonly?: boolean;
		onchange: (data: HumanTaskNodeData) => void;
		onexpand?: () => void;
	};

	let { data, readonly = false, onchange, onexpand }: Props = $props();

	function addStep() {
		onchange({
			...data,
			steps: [
				...data.steps,
				{
					id: crypto.randomUUID(),
					title: `Step ${data.steps.length + 1}`,
					blocks: []
				}
			]
		});
		onexpand?.();
	}
</script>

<div class="space-y-1.5">
	<label for="task-title" class="text-xs font-medium text-muted-foreground">Task Title</label>
	<input
		id="task-title"
		type="text"
		value={data.taskTitle}
		disabled={readonly}
		oninput={(e) =>
			onchange({
				...data,
				taskTitle: (e.currentTarget as HTMLInputElement).value
			})}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	/>
</div>

<div class="space-y-1.5">
	<label for="task-instructions" class="text-xs font-medium text-muted-foreground"
		>Instructions (Markdown)</label
	>
	<textarea
		id="task-instructions"
		value={data.instructionsMdsvex ?? ''}
		disabled={readonly}
		oninput={(e) =>
			onchange({
				...data,
				instructionsMdsvex: (e.currentTarget as HTMLTextAreaElement).value
			})}
		rows={3}
		class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none disabled:cursor-default disabled:opacity-70"
	></textarea>
</div>

<div class="space-y-2">
	<div class="flex items-center justify-between">
		<span class="text-xs font-medium text-muted-foreground">Steps</span>
		{#if !readonly}
			<button
				type="button"
				class="flex items-center gap-1 rounded-md px-2 py-0.5 text-[10px] font-medium text-primary transition-colors hover:bg-accent"
				onclick={addStep}
			>
				<Plus class="size-3" />
				Add Step
			</button>
		{/if}
	</div>

	{#each data.steps as step, stepIdx (step.id)}
		<StepEditor
			{step}
			{readonly}
			onchange={(updatedStep) => {
				const steps = [...data.steps];
				steps[stepIdx] = updatedStep;
				onchange({ ...data, steps });
			}}
			onremove={() => {
				onchange({
					...data,
					steps: data.steps.filter((_, i) => i !== stepIdx)
				});
			}}
		/>
	{/each}
</div>
