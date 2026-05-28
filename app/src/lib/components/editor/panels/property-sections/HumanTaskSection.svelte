<script lang="ts">
	import type { HumanTaskNodeData } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import Plus from '@lucide/svelte/icons/plus';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Button } from '$lib/components/ui/button';
	import StepEditor from './human-task/StepEditor.svelte';
	import InsertRefButton from './InsertRefButton.svelte';

	type Props = {
		data: HumanTaskNodeData;
		readonly?: boolean;
		binding?: YjsGraphBinding;
		nodeId?: string;
		onchange: (data: HumanTaskNodeData) => void;
		scope?: ScopeEntry[];
	};

	let {
		data,
		readonly = false,
		binding,
		nodeId,
		onchange,
		scope = []
	}: Props = $props();

	function appendToInstructions(snippet: string) {
		const curr = data.instructionsMdsvex ?? '';
		onchange({
			...data,
			instructionsMdsvex: curr ? `${curr} ${snippet}` : snippet
		});
	}

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
	}
</script>

<div class="space-y-2">
	<Label for="task-title">Task Title</Label>
	<Input
		id="task-title"
		type="text"
		value={data.taskTitle}
		disabled={readonly}
		oninput={(e) =>
			onchange({
				...data,
				taskTitle: (e.currentTarget as HTMLInputElement).value
			})}
	/>
</div>

<div class="space-y-2">
	<Label for="task-instructions">Instructions (Markdown)</Label>
	<Textarea
		id="task-instructions"
		value={data.instructionsMdsvex ?? ''}
		disabled={readonly}
		oninput={(e) =>
			onchange({
				...data,
				instructionsMdsvex: (e.currentTarget as HTMLTextAreaElement).value
			})}
		rows={4}
	/>
	{#if scope.length > 0}
		<InsertRefButton {scope} disabled={readonly} oninsert={appendToInstructions} />
	{/if}
</div>

<div class="space-y-3">
	<div class="flex items-center justify-between">
		<Label>Steps</Label>
		{#if !readonly}
			<Button variant="ghost" size="sm" onclick={addStep}>
				<Plus class="size-3.5" />
				Add Step
			</Button>
		{/if}
	</div>

	{#each data.steps as step, stepIdx (step.id)}
		<StepEditor
			{step}
			{readonly}
			{binding}
			{nodeId}
			{scope}
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
