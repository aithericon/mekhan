<script lang="ts">
	import type { HumanTaskNodeData } from '$lib/types/editor';
	import type { YjsGraphBinding } from '$lib/yjs/graph-binding.svelte';
	import type { ScopeEntry } from '$lib/editor/guard-scope';
	import { randomUuid } from '$lib/editor/ids';
	import Plus from '@lucide/svelte/icons/plus';
	import { Input } from '$lib/components/ui/input';
	import { Label } from '$lib/components/ui/label';
	import { Textarea } from '$lib/components/ui/textarea';
	import { Button } from '$lib/components/ui/button';
	import StepEditor from './human-task/StepEditor.svelte';
	import InsertRefButton from './InsertRefButton.svelte';
	import RefPicker from './RefPicker.svelte';
	import { FormField } from '$lib/components/ui/form-field';
	import * as Select from '$lib/components/ui/select';
	import { appendSnippet } from '$lib/editor/append-snippet';

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

	// `stepsRef` is the opt-in dynamic-steps source — a `<slug>.<field>` ref to
	// an upstream producer that emits form blocks at runtime. It is not yet in
	// the generated OpenAPI type, so it is accessed via a narrow cast.
	const stepsRef = $derived((data as { stepsRef?: string }).stepsRef);
	const mode = $derived(stepsRef === undefined ? 'static' : 'dynamic');

	const modeLabels: Record<string, string> = {
		static: 'Static steps — author the form here',
		dynamic: 'From upstream variable — loaded at runtime'
	};

	function setMode(next: string) {
		if (next === mode) return;
		if (next === 'dynamic') {
			// Switch to dynamic: set an empty ref (picker prompts for a field).
			// Keep the existing `steps` array intact but hidden.
			onchange({ ...data, stepsRef: '' } as HumanTaskNodeData);
		} else {
			// Switch back to static: clear the ref entirely.
			onchange({ ...data, stepsRef: undefined } as HumanTaskNodeData);
		}
	}

	function appendToInstructions(snippet: string) {
		onchange({
			...data,
			instructionsMdsvex: appendSnippet(data.instructionsMdsvex, snippet)
		});
	}

	function addStep() {
		onchange({
			...data,
			steps: [
				...data.steps,
				{
					id: randomUuid(),
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

<FormField label="Steps source" for="steps-source">
	<Select.Root
		type="single"
		value={mode}
		onValueChange={(v) => {
			if (v) setMode(v);
		}}
		disabled={readonly}
	>
		<Select.Trigger
			id="steps-source"
			class="w-full"
			disabled={readonly}
			data-testid="select-steps-source"
		>
			{modeLabels[mode] ?? modeLabels.static}
		</Select.Trigger>
		<Select.Content>
			<Select.Item value="static" label="Static steps — author the form here" />
			<Select.Item value="dynamic" label="From upstream variable — loaded at runtime" />
		</Select.Content>
	</Select.Root>
</FormField>

{#if mode === 'dynamic'}
	<div class="space-y-1.5">
		<Label class="text-sm text-muted-foreground">Steps variable (upstream)</Label>
		<RefPicker
			{scope}
			selected={stepsRef}
			placeholder="Pick a steps field…"
			allowArrayBoundary={false}
			disabled={readonly}
			onpick={(entry) => onchange({ ...data, stepsRef: entry.qualified } as HumanTaskNodeData)}
		/>
		<p class="text-sm text-muted-foreground">
			Steps are loaded from this upstream variable at runtime.
		</p>
	</div>
{:else}
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
{/if}
