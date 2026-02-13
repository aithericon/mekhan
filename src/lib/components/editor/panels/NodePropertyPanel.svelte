<script lang="ts">
	import type {
		WorkflowNodeData,
		HumanTaskNodeData,
		AutomatedStepNodeData,
		DecisionNodeData,
		LoopNodeData,
		TaskStepConfig,
		TaskFieldConfig
	} from '$lib/types/editor';
	import X from '@lucide/svelte/icons/x';
	import Plus from '@lucide/svelte/icons/plus';
	import Trash2 from '@lucide/svelte/icons/trash-2';

	type Props = {
		data: WorkflowNodeData;
		onchange: (data: WorkflowNodeData) => void;
		onclose: () => void;
	};

	let { data, onchange, onclose }: Props = $props();

	function updateField<K extends keyof WorkflowNodeData>(
		key: K,
		value: WorkflowNodeData[K]
	) {
		onchange({ ...data, [key]: value } as WorkflowNodeData);
	}

	function addStep() {
		if (data.type !== 'human_task') return;
		const updated = { ...data } as HumanTaskNodeData;
		updated.steps = [
			...updated.steps,
			{
				id: crypto.randomUUID(),
				title: `Step ${updated.steps.length + 1}`,
				blocks: []
			}
		];
		onchange(updated);
	}

	function removeStep(stepId: string) {
		if (data.type !== 'human_task') return;
		const updated = { ...data } as HumanTaskNodeData;
		updated.steps = updated.steps.filter((s) => s.id !== stepId);
		onchange(updated);
	}

	function addFieldToStep(stepId: string) {
		if (data.type !== 'human_task') return;
		const updated = { ...data } as HumanTaskNodeData;
		updated.steps = updated.steps.map((s) => {
			if (s.id !== stepId) return s;
			return {
				...s,
				blocks: [
					...s.blocks,
					{
						type: 'input' as const,
						field: {
							name: `field_${Date.now()}`,
							label: 'New Field',
							kind: 'text' as const,
							required: false
						}
					}
				]
			};
		});
		onchange(updated);
	}

	function updateStepTitle(stepId: string, title: string) {
		if (data.type !== 'human_task') return;
		const updated = { ...data } as HumanTaskNodeData;
		updated.steps = updated.steps.map((s) => (s.id === stepId ? { ...s, title } : s));
		onchange(updated);
	}

	function updateFieldConfig(stepId: string, fieldIndex: number, field: TaskFieldConfig) {
		if (data.type !== 'human_task') return;
		const updated = { ...data } as HumanTaskNodeData;
		updated.steps = updated.steps.map((s) => {
			if (s.id !== stepId) return s;
			const blocks = [...s.blocks];
			const block = blocks[fieldIndex];
			if (block && block.type === 'input') {
				blocks[fieldIndex] = { type: 'input', field };
			}
			return { ...s, blocks };
		});
		onchange(updated);
	}

	function removeField(stepId: string, fieldIndex: number) {
		if (data.type !== 'human_task') return;
		const updated = { ...data } as HumanTaskNodeData;
		updated.steps = updated.steps.map((s) => {
			if (s.id !== stepId) return s;
			const blocks = [...s.blocks];
			blocks.splice(fieldIndex, 1);
			return { ...s, blocks };
		});
		onchange(updated);
	}
</script>

<div class="flex w-80 flex-col border-l border-border bg-card" data-testid="node-property-panel">
	<div class="flex items-center justify-between border-b border-border px-3 py-2.5">
		<h2 class="text-xs font-semibold uppercase tracking-wider text-muted-foreground">
			Properties
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
				data-testid="input-node-label"
				oninput={(e) => updateField('label', (e.currentTarget as HTMLInputElement).value)}
				class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none"
			/>
		</div>

		<!-- Common: Description -->
		<div class="space-y-1.5">
			<label for="node-desc" class="text-xs font-medium text-muted-foreground">Description</label>
			<textarea
				id="node-desc"
				value={data.description ?? ''}
				data-testid="input-node-description"
				oninput={(e) => updateField('description', (e.currentTarget as HTMLTextAreaElement).value)}
				rows={2}
				class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none"
			></textarea>
		</div>

		<!-- Human Task specific -->
		{#if data.type === 'human_task'}
			{@const htData = data as HumanTaskNodeData}
			<div class="space-y-1.5">
				<label for="task-title" class="text-xs font-medium text-muted-foreground">Task Title</label>
				<input
					id="task-title"
					type="text"
					value={htData.taskTitle}
					oninput={(e) =>
						onchange({
							...htData,
							taskTitle: (e.currentTarget as HTMLInputElement).value
						})}
					class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none"
				/>
			</div>

			<div class="space-y-1.5">
				<label for="task-instructions" class="text-xs font-medium text-muted-foreground"
					>Instructions (Markdown)</label
				>
				<textarea
					id="task-instructions"
					value={htData.instructionsMdsvex ?? ''}
					oninput={(e) =>
						onchange({
							...htData,
							instructionsMdsvex: (e.currentTarget as HTMLTextAreaElement).value
						})}
					rows={3}
					class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none"
				></textarea>
			</div>

			<div class="space-y-2">
				<div class="flex items-center justify-between">
					<span class="text-xs font-medium text-muted-foreground">Steps</span>
					<button
						type="button"
						class="flex items-center gap-1 rounded-md px-2 py-0.5 text-[10px] font-medium text-primary transition-colors hover:bg-accent"
						onclick={addStep}
					>
						<Plus class="size-3" />
						Add Step
					</button>
				</div>

				{#each htData.steps as step, stepIdx (step.id)}
					<div class="rounded-lg border border-border bg-muted/30 p-2">
						<div class="mb-2 flex items-center gap-2">
							<input
								type="text"
								value={step.title}
								oninput={(e) =>
									updateStepTitle(step.id, (e.currentTarget as HTMLInputElement).value)}
								class="flex-1 rounded-md border border-input bg-background px-2 py-1 text-xs text-foreground focus:border-ring focus:outline-none"
							/>
							<button
								type="button"
								class="rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
								onclick={() => removeStep(step.id)}
							>
								<Trash2 class="size-3.5" />
							</button>
						</div>

						{#each step.blocks as block, blockIdx}
							{#if block.type === 'input'}
								<div class="mb-1.5 flex items-center gap-1 rounded border border-border/50 bg-background p-1.5 text-[10px]">
									<input
										type="text"
										value={block.field.label}
										placeholder="Label"
										oninput={(e) =>
											updateFieldConfig(step.id, blockIdx, {
												...block.field,
												label: (e.currentTarget as HTMLInputElement).value
											})}
										class="flex-1 rounded border border-input bg-background px-1.5 py-0.5 text-[10px] focus:border-ring focus:outline-none"
									/>
									<select
										value={block.field.kind}
										onchange={(e) =>
											updateFieldConfig(step.id, blockIdx, {
												...block.field,
												kind: (e.currentTarget as HTMLSelectElement).value as TaskFieldConfig['kind']
											})}
										class="rounded border border-input bg-background px-1 py-0.5 text-[10px] focus:border-ring focus:outline-none"
									>
										<option value="text">Text</option>
										<option value="textarea">Textarea</option>
										<option value="number">Number</option>
										<option value="select">Select</option>
										<option value="checkbox">Checkbox</option>
										<option value="file">File</option>
										<option value="signature">Signature</option>
									</select>
									<label class="flex items-center gap-0.5">
										<input
											type="checkbox"
											checked={block.field.required ?? false}
											onchange={(e) =>
												updateFieldConfig(step.id, blockIdx, {
													...block.field,
													required: (e.currentTarget as HTMLInputElement).checked
												})}
											class="size-3"
										/>
										<span class="text-muted-foreground">Req</span>
									</label>
									<button
										type="button"
										class="rounded p-0.5 text-muted-foreground transition-colors hover:text-destructive"
										onclick={() => removeField(step.id, blockIdx)}
									>
										<Trash2 class="size-3" />
									</button>
								</div>
							{/if}
						{/each}

						<button
							type="button"
							class="flex w-full items-center justify-center gap-1 rounded border border-dashed border-border py-1 text-[10px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
							onclick={() => addFieldToStep(step.id)}
						>
							<Plus class="size-3" />
							Add Field
						</button>
					</div>
				{/each}
			</div>
		{/if}

		<!-- Automated Step specific -->
		{#if data.type === 'automated_step'}
			{@const asData = data as AutomatedStepNodeData}
			<div class="space-y-1.5">
				<label for="backend-type" class="text-xs font-medium text-muted-foreground"
					>Backend Type</label
				>
				<select
					id="backend-type"
					value={asData.executionSpec.backendType}
					onchange={(e) =>
						onchange({
							...asData,
							executionSpec: {
								...asData.executionSpec,
								backendType: (e.currentTarget as HTMLSelectElement).value as 'python' | 'process' | 'docker'
							}
						})}
					class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none"
				>
					<option value="python">Python</option>
					<option value="process">Process</option>
					<option value="docker">Docker</option>
				</select>
			</div>
		{/if}

		<!-- Decision specific -->
		{#if data.type === 'decision'}
			{@const dData = data as DecisionNodeData}
			<div class="space-y-2">
				<div class="flex items-center justify-between">
					<span class="text-xs font-medium text-muted-foreground">Branches</span>
					<button
						type="button"
						class="flex items-center gap-1 rounded-md px-2 py-0.5 text-[10px] font-medium text-primary transition-colors hover:bg-accent"
						onclick={() =>
							onchange({
								...dData,
								conditions: [
									...dData.conditions,
									{
										edgeId: `branch-${Date.now()}`,
										label: `Branch ${dData.conditions.length + 1}`,
										guard: ''
									}
								]
							})}
					>
						<Plus class="size-3" />
						Add Branch
					</button>
				</div>

				{#each dData.conditions as condition, i (condition.edgeId)}
					<div class="rounded-lg border border-border bg-muted/30 p-2 text-[11px]">
						<input
							type="text"
							value={condition.label}
							placeholder="Branch label"
							oninput={(e) => {
								const updated = [...dData.conditions];
								updated[i] = { ...condition, label: (e.currentTarget as HTMLInputElement).value };
								onchange({ ...dData, conditions: updated });
							}}
							class="mb-1 w-full rounded border border-input bg-background px-2 py-1 text-[11px] focus:border-ring focus:outline-none"
						/>
						<input
							type="text"
							value={condition.guard}
							placeholder="Guard expression (Rhai)"
							oninput={(e) => {
								const updated = [...dData.conditions];
								updated[i] = { ...condition, guard: (e.currentTarget as HTMLInputElement).value };
								onchange({ ...dData, conditions: updated });
							}}
							class="w-full rounded border border-input bg-background px-2 py-1 font-mono text-[10px] focus:border-ring focus:outline-none"
						/>
					</div>
				{/each}

				<div class="rounded-lg border border-dashed border-border p-2 text-[11px] text-muted-foreground">
					Default branch (no guard) is always present
				</div>
			</div>
		{/if}

		<!-- Loop specific -->
		{#if data.type === 'loop'}
			{@const lData = data as LoopNodeData}
			<div class="space-y-1.5">
				<label for="max-iterations" class="text-xs font-medium text-muted-foreground"
					>Max Iterations</label
				>
				<input
					id="max-iterations"
					type="number"
					min={1}
					value={lData.maxIterations}
					oninput={(e) =>
						onchange({
							...lData,
							maxIterations: parseInt((e.currentTarget as HTMLInputElement).value) || 1
						})}
					class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 text-sm text-foreground focus:border-ring focus:outline-none"
				/>
			</div>
			<div class="space-y-1.5">
				<label for="loop-condition" class="text-xs font-medium text-muted-foreground"
					>Loop Condition (Rhai)</label
				>
				<input
					id="loop-condition"
					type="text"
					value={lData.loopCondition}
					oninput={(e) =>
						onchange({
							...lData,
							loopCondition: (e.currentTarget as HTMLInputElement).value
						})}
					class="w-full rounded-md border border-input bg-background px-2.5 py-1.5 font-mono text-sm text-foreground focus:border-ring focus:outline-none"
				/>
			</div>
		{/if}
	</div>
</div>
